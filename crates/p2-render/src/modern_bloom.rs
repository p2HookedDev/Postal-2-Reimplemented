//! Modern graphics: bloom with Bevy's look in two passes, where Bevy's takes a render pass per mip and step. One compute
//! pass runs the whole downsample and upsample chain, one render pass adds the result to the frame.

use bevy::app::{App, Plugin};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::Hdr;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::query::QueryItem;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::lifetimeless::Read;
use bevy::math::{UVec2, UVec4, Vec4, ops};
use bevy::render::camera::ExtractedCamera;
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::{
    AddressMode, BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, BufferId,
    CachedComputePipelineId, CachedRenderPipelineId, ColorTargetState, ColorWrites, ComputePassDescriptor, ComputePipelineDescriptor, Extent3d, FilterMode,
    FragmentState, PipelineCache, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType,
    SpecializedRenderPipeline, SpecializedRenderPipelines, StorageTextureAccess, Texture, TextureDescriptor, TextureDimension, TextureFormat, TextureId,
    TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor, TextureViewDimension, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

use crate::screen_pass::{PassUniforms, ViewBindGroups};
use crate::shaders::add_shaders;

const BLOOM_SHADER: Handle<Shader> = uuid_handle!("3b8e5d27-a6c1-4f94-8d02-7e1c9a54b6f3");
/// Bevy's `Rg11b10Ufloat` cannot be a storage texture.
const BLOOM_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
const MAX_MIP_DIMENSION: u32 = 512;
const MIP_COUNT: u32 = MAX_MIP_DIMENSION.ilog2() - 1;
const MIPS: usize = MIP_COUNT as usize;
const WORKGROUP: u32 = 8;
// The texture's layers: the downsampled mips, and the upsample's sums, as a compute step cannot blend into the mip it reads.
const DOWN: u32 = 0;
const UP: u32 = 1;

/// Adds the bloom and keeps the frame's alpha, the sun rays' mask.
const ADD_KEEP_ALPHA: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

pub struct ModernBloomPlugin;

impl Plugin for ModernBloomPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(BLOOM_SHADER, Shader::from_wesl(include_str!("modern_bloom.wesl"), "p2_render/modern_bloom.wesl"))]);
        app.add_plugins(ExtractComponentPlugin::<ModernBloom>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<BloomPipeline>>()
            .init_gpu_resource::<PassUniforms<BloomStep>>()
            .add_systems(RenderStartup, init_bloom_pipeline)
            .add_systems(Render, (prepare_bloom_views.in_set(RenderSystems::PrepareResources), prepare_bloom_chains.in_set(RenderSystems::PrepareBindGroups)))
            .add_systems(Core3d, draw_bloom.before(tonemapping).in_set(Core3dSystems::PostProcess));
    }
}

/// Bevy's `Bloom` settings in its additive mode at uniform scale.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
#[require(Hdr)]
pub struct ModernBloom {
    pub intensity: f32,
    pub low_frequency_boost: f32,
    pub low_frequency_boost_curvature: f32,
    pub high_pass_frequency: f32,
    pub threshold: f32,
    pub threshold_softness: f32,
}

impl ModernBloom {
    /// How much of mip `mip`'s blur is added to the level above it; mip 0's goes onto the frame.
    fn blend(&self, mip: f32) -> f32 {
        let along = mip / (MIP_COUNT - 1) as f32;
        let boost = (1.0 - ops::powf(1.0 - along, 1.0 / (1.0 - self.low_frequency_boost_curvature))) * self.low_frequency_boost;
        let high_pass = 1.0 - ((along - self.high_pass_frequency) / self.high_pass_frequency).clamp(0.0, 1.0);
        (self.intensity + boost) * high_pass
    }

    fn threshold_curve(&self) -> Vec4 {
        let knee = self.threshold * self.threshold_softness.clamp(0.0, 1.0);
        Vec4::new(self.threshold, self.threshold - knee, 2.0 * knee, 0.25 / (knee + 0.00001))
    }
}

impl SyncComponent<RenderApp> for ModernBloom {
    type Target = (Self, BloomView, BloomChain);
}

impl ExtractComponent<RenderApp> for ModernBloom {
    type QueryData = Read<Self>;
    type QueryFilter = With<Hdr>;
    type Out = Self;

    fn extract_component(bloom: QueryItem<'_, '_, Self::QueryData>) -> Option<Self::Out> {
        Some(*bloom)
    }
}

/// One per mip of each view, the mip's blend; the downsamples read the first.
#[derive(ShaderType, Debug, Clone, Copy, PartialEq)]
pub struct BloomStep {
    threshold: Vec4,
    viewport: Vec4,
    blend: f32,
}

#[derive(Resource)]
pub struct BloomPipeline {
    down_layout: BindGroupLayoutDescriptor,
    up_layout: BindGroupLayoutDescriptor,
    composite_layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    first: CachedComputePipelineId,
    down: CachedComputePipelineId,
    up: CachedComputePipelineId,
}

fn init_bloom_pipeline(mut commands: Commands, render_device: Res<RenderDevice>, pipeline_cache: Res<PipelineCache>, fullscreen: Res<FullscreenShader>) {
    let sampled = || binding_types::texture_2d(TextureSampleType::Float { filterable: true });
    let filtering = || binding_types::sampler(SamplerBindingType::Filtering);
    let step = || binding_types::uniform_buffer::<BloomStep>(true);
    let written = || binding_types::texture_storage_2d(BLOOM_FORMAT, StorageTextureAccess::WriteOnly);
    let down_layout =
        BindGroupLayoutDescriptor::new("bloom downsample", &BindGroupLayoutEntries::sequential(ShaderStages::COMPUTE, (sampled(), filtering(), step(), written())));
    let up_layout = BindGroupLayoutDescriptor::new(
        "bloom upsample",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (sampled(), filtering(), step(), written(), binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
        ),
    );
    let composite_layout = BindGroupLayoutDescriptor::new("bloom composite", &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (sampled(), filtering(), step())));
    let sampler = render_device.create_sampler(&SamplerDescriptor {
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        ..Default::default()
    });
    let compute = |layout: &BindGroupLayoutDescriptor, entry: &'static str| {
        pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some(entry.into()),
            layout: vec![layout.clone()],
            shader: BLOOM_SHADER,
            entry_point: Some(entry.into()),
            ..Default::default()
        })
    };
    let (first, down, up) = (compute(&down_layout, "downsample_first"), compute(&down_layout, "downsample"), compute(&up_layout, "upsample"));
    commands.insert_resource(BloomPipeline { down_layout, up_layout, composite_layout, sampler, fullscreen: fullscreen.clone(), first, down, up });
}

impl SpecializedRenderPipeline for BloomPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("bloom composite".into()),
            layout: vec![self.composite_layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: BLOOM_SHADER,
                entry_point: Some("composite".into()),
                targets: vec![Some(ColorTargetState { format, blend: Some(ADD_KEEP_ALPHA), write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

#[derive(Component)]
pub struct BloomView {
    texture: CachedTexture,
    size: UVec2,
    offsets: [u32; MIPS],
    composite: CachedRenderPipelineId,
}

/// Made again only when the texture or the uniform buffer changes.
#[derive(Component)]
pub struct BloomChain {
    key: (TextureId, BufferId),
    first_output: TextureView,
    down: Vec<BindGroup>,
    /// From the smallest mip up.
    up: Vec<BindGroup>,
    composite: BindGroup,
}

/// The texture is as tall as `MAX_MIP_DIMENSION` whatever the viewport's size, as Bevy's is.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn prepare_bloom_views(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<BloomPipeline>>,
    pipeline: Res<BloomPipeline>,
    mut texture_cache: ResMut<TextureCache>,
    mut uniforms: ResMut<PassUniforms<BloomStep>>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    views: Query<(Entity, &ExtractedView, &ExtractedCamera, &ModernBloom)>,
) {
    let Some(mut writer) = uniforms.0.get_writer(views.iter().len() * MIPS, &render_device, &render_queue) else {
        return;
    };
    for (entity, view, camera, bloom) in &views {
        let size = UVec2::new(view.viewport.z, view.viewport.w);
        let Some(target) = camera.physical_target_size.filter(|_| size.min_element() > 0) else {
            continue;
        };
        let viewport = view.viewport.as_vec4() / UVec4::new(target.x, target.y, target.x, target.y).as_vec4();
        let threshold = bloom.threshold_curve();
        let offsets = std::array::from_fn(|mip| writer.write(&BloomStep { threshold, viewport, blend: bloom.blend(mip as f32) }));
        let texture_size = (size.as_vec2() * (MAX_MIP_DIMENSION as f32 / size.y as f32)).round().as_uvec2().max(UVec2::ONE);
        let texture = texture_cache.get(
            &render_device,
            TextureDescriptor {
                label: Some("bloom"),
                size: Extent3d { width: texture_size.x, height: texture_size.y, depth_or_array_layers: 2 },
                mip_level_count: MIP_COUNT,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: BLOOM_FORMAT,
                usage: TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );
        let composite = pipelines.specialize(&pipeline_cache, &pipeline, view.target_format);
        commands.entity(entity).insert(BloomView { texture, size: texture_size, offsets, composite });
    }
}

fn mip_view(texture: &Texture, layer: u32, mip: u32) -> TextureView {
    texture.create_view(&TextureViewDescriptor {
        label: Some("bloom mip"),
        dimension: Some(TextureViewDimension::D2),
        base_mip_level: mip,
        mip_level_count: Some(1),
        base_array_layer: layer,
        array_layer_count: Some(1),
        ..Default::default()
    })
}

/// Each step's views are single mips, so no step reads the mip it writes.
fn prepare_bloom_chains(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<BloomPipeline>,
    uniforms: Res<PassUniforms<BloomStep>>,
    views: Query<(Entity, &BloomView, Option<&BloomChain>)>,
) {
    let (Some(uniform), Some(buffer)) = (uniforms.0.binding(), uniforms.0.buffer().map(|buffer| buffer.id())) else {
        return;
    };
    for (entity, view, chain) in &views {
        let texture = &view.texture.texture;
        let key = (texture.id(), buffer);
        if chain.is_some_and(|chain| chain.key == key) {
            continue;
        }
        let down_views: Vec<TextureView> = (0..MIP_COUNT).map(|mip| mip_view(texture, DOWN, mip)).collect();
        let up_views: Vec<TextureView> = (0..MIP_COUNT - 1).map(|mip| mip_view(texture, UP, mip)).collect();
        let (Some(first_output), Some(mut source), Some(top)) = (down_views.first(), down_views.last(), up_views.first()) else {
            continue;
        };
        let down_layout = pipeline_cache.get_bind_group_layout(&pipeline.down_layout);
        let down = down_views
            .iter()
            .zip(down_views.iter().skip(1))
            .map(|(input, output)| {
                render_device.create_bind_group("bloom downsample", &down_layout, &BindGroupEntries::sequential((input, &pipeline.sampler, uniform.clone(), output)))
            })
            .collect();
        let up_layout = pipeline_cache.get_bind_group_layout(&pipeline.up_layout);
        let mut up = Vec::with_capacity(up_views.len());
        for (output, base) in up_views.iter().zip(&down_views).rev() {
            up.push(render_device.create_bind_group(
                "bloom upsample",
                &up_layout,
                &BindGroupEntries::sequential((source, &pipeline.sampler, uniform.clone(), output, base)),
            ));
            source = output;
        }
        let composite = render_device.create_bind_group(
            "bloom composite",
            &pipeline_cache.get_bind_group_layout(&pipeline.composite_layout),
            &BindGroupEntries::sequential((top, &pipeline.sampler, uniform.clone())),
        );
        commands.entity(entity).insert(BloomChain { key, first_output: first_output.clone(), down, up, composite });
    }
}

fn draw_bloom(
    view: ViewQuery<(&ViewTarget, &ExtractedCamera, &BloomView, &BloomChain)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<BloomPipeline>,
    uniforms: Res<PassUniforms<BloomStep>>,
    mut scene_groups: Local<ViewBindGroups<(TextureViewId, (TextureId, BufferId))>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, camera, prepared, chain) = view.into_inner();
    let (Some(first), Some(down), Some(up), Some(composite), Some(uniform)) = (
        pipeline_cache.get_compute_pipeline(pipeline.first),
        pipeline_cache.get_compute_pipeline(pipeline.down),
        pipeline_cache.get_compute_pipeline(pipeline.up),
        pipeline_cache.get_render_pipeline(prepared.composite),
        uniforms.0.binding(),
    ) else {
        return;
    };
    let scene = target.main_texture_view();
    let device = ctx.render_device();
    let from_scene = scene_groups.get_or_make(entity, (scene.id(), chain.key), || {
        device.create_bind_group(
            "bloom first downsample",
            &pipeline_cache.get_bind_group_layout(&pipeline.down_layout),
            &BindGroupEntries::sequential((scene, &pipeline.sampler, uniform, &chain.first_output)),
        )
    });
    let [frame_offset, upper_offsets @ ..] = prepared.offsets;
    let downs = chain.down.iter().zip(1..).map(|(group, mip)| (down, group, frame_offset, mip));
    let ups = chain.up.iter().zip(upper_offsets.into_iter().rev()).zip((0..MIP_COUNT - 1).rev()).map(|((group, offset), mip)| (up, group, offset, mip));
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = diagnostics.time_span(ctx.command_encoder(), "bloom");
    {
        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor { label: Some("bloom"), timestamp_writes: None });
        for (compute, group, offset, mip) in std::iter::once((first, from_scene, frame_offset, 0)).chain(downs).chain(ups) {
            let groups = ((prepared.size >> mip).max(UVec2::ONE) + UVec2::splat(WORKGROUP - 1)) / WORKGROUP;
            pass.set_pipeline(compute);
            pass.set_bind_group(0, group, &[offset]);
            pass.dispatch_workgroups(groups.x, groups.y, 1);
        }
    }
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("bloom composite"),
            color_attachments: &[Some(target.get_unsampled_color_attachment())],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(viewport) = camera.viewport.as_ref() {
            pass.set_camera_viewport(viewport);
        }
        pass.set_render_pipeline(composite);
        pass.set_bind_group(0, &chain.composite, &[frame_offset]);
        pass.draw(0..3, 0..1);
    }
    span.end(ctx.command_encoder());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn workgroup_matches_the_shader() {
        assert_eq!(shader_const(include_str!("modern_bloom.wesl"), "WORKGROUP"), f64::from(WORKGROUP));
    }
}
