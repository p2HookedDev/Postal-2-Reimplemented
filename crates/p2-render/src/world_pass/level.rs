//! A cached sun level is drawn on its redraw frame as Bevy would draw its shadow view, into a scratch depth
//! target, then copied into its tile of our atlas. The record row follows the levels every frame.

use super::*;
use super::cull::cull_world;
use super::draw::{draw_bucket, material_group};
use super::queue::{shadow_pipelines, with_buckets};
use bevy::camera::Exposure;
use bevy::diagnostic::FrameCount;
use bevy::pbr::PrepassViewBindGroup;
use bevy::render::globals::GlobalsBuffer;
use bevy::render::mesh::RenderMeshBufferInfo;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, texture_depth_2d};
use bevy::render::render_resource::{
    BufferBinding, BufferInitDescriptor, CachedRenderPipelineId, CommandEncoder, RenderPipeline, ColorTargetState, ColorWrites, CompareFunction, DepthStencilState, DynamicUniformBuffer, Extent3d,
    FragmentState, LoadOp, MultisampleState, Operations, Origin3d, PrimitiveState, RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor,
    RenderPipelineDescriptor, SamplerBindingType, StoreOp, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDescriptor, TextureDimension, TextureFormat,
    TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor, VertexState,
};
use bevy::render::view::{RenderVisibilityRanges, ViewUniform};
use bevy::shader::ShaderDefVal;

use crate::sun_levels::{AtlasLayout, LEVEL_CASTER_SHADER, PropCaster, SunLevelAtlas, SunLevelFrame, SunLevelProps, SunLevelRecord, texel_bytes};

const CASTER_STRIDE: u64 = 256;
const CASTER_SIZE: u64 = 144;
const LEVEL_LIGHT_KEY: MeshPipelineKey = MeshPipelineKey::DEPTH_PREPASS.union(MeshPipelineKey::VIEW_PROJECTION_ORTHOGRAPHIC).union(MeshPipelineKey::UNCLIPPED_DEPTH_ORTHO);
const EMULATION: &str = "UNCLIPPED_DEPTH_ORTHO_EMULATION";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Cut {
    Opaque,
    Masked,
}

#[derive(Default)]
pub(super) struct LevelDraw {
    pub(super) slot: Option<u32>,
    pipelines: Vec<(usize, CachedRenderPipelineId, bool)>,
    pipelines_for: Option<Entity>,
}

pub(super) fn queue_level(world: &mut World) {
    let drawing = world.get_resource::<SunLevelFrame>().is_some_and(|frame| frame.drawing.is_some());
    let casts = world.get_resource::<WorldCasts>().is_some_and(|casts| casts.0);
    let first = world.resource::<WorldPass>().built.as_ref().and_then(|built| built.buckets.first()).map(|bucket| bucket.entity);
    if !drawing || !casts || first.is_none() || world.resource::<WorldPass>().level.pipelines_for == first {
        return;
    }
    let Some(pipelines) = with_buckets(world, |world, buckets| shadow_pipelines(world, buckets, LEVEL_LIGHT_KEY)) else { return };
    let mut pass = world.resource_mut::<WorldPass>();
    pass.level.pipelines = pipelines;
    pass.level.pipelines_for = first;
}

struct Layouts {
    caster: BindGroupLayoutDescriptor,
    cut: BindGroupLayoutDescriptor,
    copy: BindGroupLayoutDescriptor,
}

fn level_layouts() -> Layouts {
    Layouts {
        caster: BindGroupLayoutDescriptor::new("level_caster", &BindGroupLayoutEntries::single(ShaderStages::VERTEX_FRAGMENT, uniform_buffer_sized(true, NonZero::new(CASTER_SIZE)))),
        cut: BindGroupLayoutDescriptor::new(
            "level_caster_cut",
            &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering))),
        ),
        copy: BindGroupLayoutDescriptor::new("level_copy", &BindGroupLayoutEntries::with_indices(ShaderStages::FRAGMENT, ((1, texture_depth_2d()),))),
    }
}

fn prop_pipeline(cache: &PipelineCache, layouts: &Layouts, layout: &MeshVertexBufferLayoutRef, cut: Cut, emulated: bool) -> Option<CachedRenderPipelineId> {
    let (attributes, vertex, fragment, groups) = match cut {
        Cut::Opaque => (vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0)], "prop_vertex", emulated.then_some("prop_fragment"), vec![layouts.caster.clone()]),
        Cut::Masked => (
            vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_UV_0.at_shader_location(1)],
            "prop_vertex_uv",
            Some("prop_fragment_masked"),
            vec![layouts.caster.clone(), layouts.cut.clone()],
        ),
    };
    let buffer = layout.0.get_layout(&attributes).ok()?;
    let shader_defs: Vec<ShaderDefVal> = if emulated { vec![EMULATION.into()] } else { Vec::new() };
    Some(cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("sun level prop".into()),
        layout: groups,
        vertex: VertexState { shader: LEVEL_CASTER_SHADER, entry_point: Some(vertex.into()), buffers: vec![buffer], shader_defs: shader_defs.clone(), ..Default::default() },
        fragment: fragment.map(|entry| FragmentState { shader: LEVEL_CASTER_SHADER, entry_point: Some(entry.into()), shader_defs, targets: Vec::new(), ..Default::default() }),
        primitive: PrimitiveState { unclipped_depth: !emulated, cull_mode: None, ..Default::default() },
        depth_stencil: Some(DepthStencilState {
            format: TextureFormat::Depth32Float,
            depth_write_enabled: true.into(),
            depth_compare: CompareFunction::GreaterEqual.into(),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: MultisampleState::default(),
        ..Default::default()
    }))
}

fn copy_pipeline(cache: &PipelineCache, layouts: &Layouts, format: TextureFormat) -> CachedRenderPipelineId {
    let entry = if format == TextureFormat::R32Float { "copy_fragment" } else { "copy_fragment_packed" };
    cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("sun level copy".into()),
        layout: vec![layouts.copy.clone()],
        vertex: VertexState { shader: LEVEL_CASTER_SHADER, entry_point: Some("copy_vertex".into()), ..Default::default() },
        fragment: Some(FragmentState {
            shader: LEVEL_CASTER_SHADER,
            entry_point: Some(entry.into()),
            targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn prop_uniform(bytes: &mut Vec<u8>, clip_from_world: Mat4, prop: &PropCaster) {
    let start = bytes.len();
    let cut = [prop.cut.map_or(0.0, |(_, cutoff)| cutoff), 0.0, 0.0, 0.0];
    bytes.extend(clip_from_world.to_cols_array().iter().chain(&prop.world_from_local.to_cols_array()).chain(&cut).flat_map(|value| value.to_le_bytes()));
    bytes.resize(start + CASTER_STRIDE as usize, 0);
}

#[derive(Default)]
pub(super) struct LevelState {
    layouts: Option<Layouts>,
    view: DynamicUniformBuffer<ViewUniform>,
    scratch: Option<(u32, TextureView)>,
    props: HashMap<(MeshVertexBufferLayoutRef, Cut), Option<CachedRenderPipelineId>>,
    copy: Option<(TextureFormat, CachedRenderPipelineId)>,
    written: Option<(AssetId<Image>, Vec<f32>)>,
}

#[derive(SystemParam)]
pub(super) struct LevelBindings<'w, 's> {
    prepass: Res<'w, PrepassPipeline>,
    prepass_views: Res<'w, PrepassViewBindGroup>,
    globals: Res<'w, GlobalsBuffer>,
    ranges: Res<'w, RenderVisibilityRanges>,
    frame_count: Res<'w, FrameCount>,
    stale: Option<Res<'w, StaticShadowsStale>>,
    cameras: Query<'w, 's, &'static ExtractedView, With<WorldCamera>>,
}

#[derive(SystemParam)]
pub(super) struct LevelSources<'w, 's> {
    pass: Res<'w, WorldPass>,
    frame: Option<Res<'w, SunLevelFrame>>,
    atlas: Option<Res<'w, SunLevelAtlas>>,
    props: Option<Res<'w, SunLevelProps>>,
    images: Res<'w, RenderAssets<GpuImage>>,
    meshes: Res<'w, RenderAssets<RenderMesh>>,
    allocator: Res<'w, MeshAllocator>,
    materials: Res<'w, MaterialBindGroupAllocators>,
    cache: Res<'w, PipelineCache>,
    device: Res<'w, RenderDevice>,
    queue: Res<'w, RenderQueue>,
    bindings: LevelBindings<'w, 's>,
    state: Local<'s, LevelState>,
}

fn level_view(record: &SunLevelRecord, size: u32, camera: Option<&ExtractedView>, frame_count: u32) -> ViewUniform {
    let world_from_view = record.world_from_view;
    let view_from_world = world_from_view.inverse();
    let view_from_clip = record.clip_from_view.inverse();
    let viewport = Vec4::new(0.0, 0.0, size as f32, size as f32);
    ViewUniform {
        clip_from_world: record.clip_from_world,
        unjittered_clip_from_world: record.clip_from_view * view_from_world,
        world_from_clip: world_from_view * view_from_clip,
        world_from_view,
        view_from_world,
        clip_from_view: record.clip_from_view,
        view_from_clip,
        world_position: world_from_view.w_axis.truncate(),
        exposure: Exposure::default().exposure(),
        viewport,
        main_pass_viewport: viewport,
        frustum: [Vec4::ZERO; 6],
        lod_view_world_position: camera.map_or(Vec3::ZERO, |camera| camera.world_from_view.translation()),
        color_grading: camera.map(|camera| camera.color_grading.clone()).unwrap_or_default().into(),
        mip_bias: 0.0,
        frame_count,
    }
}

pub(super) fn cull_and_draw_level(_view: ViewQuery<Entity, With<WorldCamera>>, sources: LevelSources, mut context: RenderContext) {
    cull_world(&sources.pass, &sources.cache, &mut context);
    draw_level(sources, &mut context);
}

fn draw_level(sources: LevelSources, context: &mut RenderContext<'_, '_>) {
    let LevelSources { pass, frame, atlas, props, images, meshes, allocator, materials, cache, device, queue, bindings, mut state } = sources;
    let (Some(frame), Some(atlas)) = (frame, atlas) else { return };
    let (Some(image), Some((layout, format))) = (&atlas.image, atlas.made) else { return };
    let redraw_later = || if frame.drawing.is_some() && let Some(stale) = &bindings.stale { stale.mark() };
    let Some(target) = images.get(image).filter(|_| layout.tiles as usize == frame.records.len() && layout.tile_size == frame.map_size) else {
        redraw_later();
        return;
    };
    let state = &mut *state;
    state.write_records(&queue, target, image.id(), &layout, &frame.records);
    let Some(drawing) = frame.drawing else { return };
    let world = pass.built.as_ref().zip(pass.level.slot);
    let record = frame.records[drawing];
    let size = frame.map_size;
    let emulated = !bindings.prepass.depth_clip_control_supported;
    let Some(view_group) = state.view_group(&device, &queue, &cache, &bindings, &record, size) else { return };
    state.make_scratch(&device, size);
    let layouts = state.layouts.get_or_insert_with(level_layouts);
    let Some((_, scratch)) = &state.scratch else { return };

    let (world_draws, mut waiting) =
        world.map_or_else(|| (Vec::new(), false), |(built, _)| pass.level.world_draws(built, &materials, &cache, &bindings.prepass_views.empty_bind_group));

    let cut_layout = cache.get_bind_group_layout(&layouts.cut);
    let mut bytes = Vec::new();
    let mut cut_groups = Vec::new();
    let mut prop_draws = Vec::new();
    for prop in props.as_deref().map_or(&[][..], |props| &props.0) {
        let cut = if prop.cut.is_some() { Cut::Masked } else { Cut::Opaque };
        let Some(mesh) = meshes.get(prop.mesh) else {
            waiting = true;
            continue;
        };
        let Some(id) = *state.props.entry((mesh.layout.clone(), cut)).or_insert_with(|| prop_pipeline(&cache, layouts, &mesh.layout, cut, emulated)) else { continue };
        let (Some(pipeline), Some(vertices)) = (cache.get_render_pipeline(id), allocator.mesh_vertex_slice(&prop.mesh)) else {
            waiting = true;
            continue;
        };
        let indices = match mesh.buffer_info {
            RenderMeshBufferInfo::Indexed { count, index_format } => match allocator.mesh_index_slice(&prop.mesh) {
                Some(indices) => Some((indices, index_format, count)),
                None => {
                    waiting = true;
                    continue;
                }
            },
            RenderMeshBufferInfo::NonIndexed => None,
        };
        let group = match prop.cut.map(|(texture, _)| images.get(texture)) {
            Some(Some(image)) => {
                cut_groups.push(device.create_bind_group(None, &cut_layout, &BindGroupEntries::sequential((&image.texture_view, &image.sampler))));
                Some(cut_groups.len() - 1)
            }
            Some(None) => {
                waiting = true;
                continue;
            }
            None => None,
        };
        let offset = bytes.len() as u32;
        prop_uniform(&mut bytes, record.clip_from_world, prop);
        prop_draws.push((pipeline, offset, group, vertices, indices));
    }
    let caster_group = (!bytes.is_empty()).then(|| {
        let buffer = device.create_buffer_with_data(&BufferInitDescriptor { label: Some("sun level props"), contents: &bytes, usage: BufferUsages::UNIFORM });
        device.create_bind_group(None, &cache.get_bind_group_layout(&layouts.caster), &BindGroupEntries::single(BufferBinding { buffer: &buffer, offset: 0, size: NonZero::new(CASTER_SIZE) }))
    });

    let copy_id = match state.copy {
        Some((made, id)) if made == format => id,
        _ => state.copy.insert((format, copy_pipeline(&cache, layouts, format))).1,
    };
    if waiting && let Some(stale) = &bindings.stale {
        stale.mark();
    }
    let Some(copy) = cache.get_render_pipeline(copy_id) else {
        if let Some(stale) = &bindings.stale {
            stale.mark();
        }
        return;
    };
    let copy_group = device.create_bind_group("sun level copy", &cache.get_bind_group_layout(&layouts.copy), &BindGroupEntries::with_indices(((1, scratch),)));

    let encoder = context.command_encoder();
    {
        let raw = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("sun level"),
            color_attachments: &[],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment { view: scratch, depth_ops: Some(Operations { load: LoadOp::Clear(0.0), store: StoreOp::Store }), stencil_ops: None }),
            ..Default::default()
        });
        let mut depth = TrackedRenderPass::new(&device, raw);
        if let Some((built, slot)) = world {
            for &(index, pipeline, group) in &world_draws {
                depth.set_render_pipeline(pipeline);
                depth.set_bind_group(0, &view_group, &[0]);
                depth.set_bind_group(1, &bindings.prepass_views.empty_bind_group, &[]);
                depth.set_bind_group(2, &built.mesh_groups[built.buckets[index].mesh_group], &[]);
                depth.set_bind_group(3, group, &[]);
                draw_bucket(&mut depth, built, &allocator, index, slot);
            }
        }
        if let Some(casters) = &caster_group {
            for (pipeline, offset, group, vertices, indices) in &prop_draws {
                depth.set_render_pipeline(pipeline);
                depth.set_bind_group(0, casters, &[*offset]);
                if let Some(group) = group {
                    depth.set_bind_group(1, &cut_groups[*group], &[]);
                }
                depth.set_vertex_buffer(0, vertices.buffer.slice(..));
                match indices {
                    Some((indices, format, count)) => {
                        depth.set_index_buffer(indices.buffer.slice(..), *format);
                        depth.draw_indexed(indices.range.start..indices.range.start + count, vertices.range.start as i32, 0..1);
                    }
                    None => depth.draw(vertices.range.clone(), 0..1),
                }
            }
        }
    }
    copy_to_tile(encoder, &device, &target.texture_view, (copy, &copy_group), layout.tile_corner(drawing as u32), size);
}

/// The scratch depth into its tile, bit for bit: a texture copy cannot go from depth to colour.
fn copy_to_tile(encoder: &mut CommandEncoder, device: &RenderDevice, atlas: &TextureView, (copy, group): (&RenderPipeline, &BindGroup), [x, y]: [u32; 2], size: u32) {
    let raw = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("sun level copy"),
        color_attachments: &[Some(RenderPassColorAttachment { view: atlas, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Load, store: StoreOp::Store } })],
        ..Default::default()
    });
    let mut tile = TrackedRenderPass::new(device, raw);
    tile.set_viewport(x as f32, y as f32, size as f32, size as f32, 0.0, 1.0);
    tile.set_scissor_rect(x, y, size, size);
    tile.set_render_pipeline(copy);
    tile.set_bind_group(0, group, &[]);
    tile.draw(0..3, 0..1);
}

impl LevelDraw {
    fn world_draws<'w>(&self, built: &'w Built, materials: &'w MaterialBindGroupAllocators, cache: &'w PipelineCache, empty: &'w BindGroup) -> (Vec<(usize, &'w RenderPipeline, &'w BindGroup)>, bool) {
        let mut waiting = false;
        let mut draws = Vec::new();
        for &(index, id, only_depth) in &self.pipelines {
            let group = if only_depth { Some(empty) } else { material_group(built, materials, index) };
            match (cache.get_render_pipeline(id), group) {
                (Some(pipeline), Some(group)) => draws.push((index, pipeline, group)),
                _ => waiting = true,
            }
        }
        (draws, waiting)
    }
}

impl LevelState {
    /// Only when a record changed: the row is read by every lit pixel, so a write each frame would cost for nothing.
    fn write_records(&mut self, queue: &RenderQueue, target: &GpuImage, image: AssetId<Image>, layout: &AtlasLayout, records: &[SunLevelRecord]) {
        let texels = layout.record_texels(records);
        if self.written.as_ref().is_some_and(|(id, old)| *id == image && *old == texels) {
            return;
        }
        let bytes = texel_bytes(&texels);
        queue.write_texture(
            TexelCopyTextureInfo { texture: &target.texture, mip_level: 0, origin: Origin3d { x: 0, y: layout.record_row(), z: 0 }, aspect: TextureAspect::All },
            &bytes,
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes.len() as u32), rows_per_image: None },
            Extent3d { width: texels.len() as u32, height: 1, depth_or_array_layers: 1 },
        );
        self.written = Some((image, texels));
    }

    fn view_group(&mut self, device: &RenderDevice, queue: &RenderQueue, cache: &PipelineCache, bindings: &LevelBindings<'_, '_>, record: &SunLevelRecord, size: u32) -> Option<BindGroup> {
        self.view.clear();
        self.view.push(&level_view(record, size, bindings.cameras.iter().next(), bindings.frame_count.0));
        self.view.write_buffer(device, queue);
        let layout = cache.get_bind_group_layout(&bindings.prepass.view_layout_no_motion_vectors);
        let (Some(view), Some(globals), Some(ranges)) = (self.view.binding(), bindings.globals.buffer.binding(), bindings.ranges.buffer().buffer()) else { return None };
        Some(device.create_bind_group("sun level view", &layout, &BindGroupEntries::with_indices(((0, view), (1, globals), (14, ranges.as_entire_binding())))))
    }

    fn make_scratch(&mut self, device: &RenderDevice, size: u32) {
        if self.scratch.as_ref().is_some_and(|(side, _)| *side == size) {
            return;
        }
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("sun level scratch"),
            size: Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Depth32Float,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.scratch = Some((size, texture.create_view(&TextureViewDescriptor::default())));
    }
}
