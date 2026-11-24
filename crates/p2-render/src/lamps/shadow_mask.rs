//! The lamps' and the sun's shadows of each pixel of the world camera, cast after its depth prepass and the sun's
//! cascades (`lamp_mask.wesl`) into an image the lit materials hold in their normal map slot, so a surface runs the
//! shadow code only for the lamps the image does not hold (`lamp_shadow.wesl` `screen_shadows`) and none for the sun
//! (`screen_sun`). Run inside the lit surfaces' shader, the shadows held up the rest of its work in a fight.

use bevy::app::{App, PostUpdate};
use bevy::asset::{Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::camera::{Camera, CameraUpdateSystems};
use bevy::core_pipeline::prepass::{NormalPrepass, ViewPrepassTextures};
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::prelude::{Commands, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::ShadowFilteringMethod;
use bevy::math::UVec2;
use bevy::pbr::contact_shadows::{ContactShadows, ContactShadowsBuffer, ContactShadowsUniform, ViewContactShadowsUniformOffset};
use bevy::pbr::{GpuLights, LATE_SHADOW_PASS, LightMeta, ShadowSamplers, ViewLightsUniformOffset, ViewShadowBindings, per_view_shadow_pass};
use bevy::platform::collections::HashMap;
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BufferId, CachedComputePipelineId, ComputePassDescriptor,
    ComputePipeline, ComputePipelineDescriptor, Extent3d, PipelineCache, SamplerBindingType, ShaderStages, StorageTextureAccess, TextureDimension, TextureFormat,
    TextureSampleType, TextureUsages, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderQueue, ViewQuery};
use bevy::render::texture::{FallbackImage, GpuImage};
use bevy::render::view::{Msaa, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use p2_host::display::{MODERN_SHADOW_FILTER, Settings};

use super::atlas::write_rows;
use super::{AtlasSizeSystems, LampAtlas, MaterialAtlas, MaterialSlot, follow_atlas};
use crate::shaders::{add_shaders, shader_file};
use crate::stage_encoder::{BeforeMain, BeforeMainPasses, StageEncoder};
use crate::sun_levels::SunLevelAtlas;
use crate::world_pass::WorldCamera;

const MASK_SHADER: Handle<Shader> = uuid_handle!("f3a4516c-ba33-4e97-9d69-3ce25bfa5f3f");
const GROUP_SIDE: u32 = 8;
// The sun's rows start this many rows past the pixels' (the header between), a row for this many pixel rows, one in
// each channel. Matches `lamp_shadow.wesl`.
const SUN_ROWS_FROM: u32 = 1;
const SUN_PIXELS: u32 = 4;

/// A texel a pixel of the world camera's target, a header row after them and the sun's rows (`mask_height`), made anew
/// when the target changes size; none without a normal prepass, or with MSAA, where every surface casts its own.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub(super) struct ShadowMask {
    image: Option<Handle<Image>>,
    size: UVec2,
    /// The shadow filter level: 0 sharp, 1 soft, 2 softer, 3 contact hardening.
    softness: u8,
}

impl MaterialAtlas for ShadowMask {
    const SLOT: MaterialSlot = MaterialSlot::NormalMap;

    fn wanted(&self) -> Option<Handle<Image>> {
        self.image.clone()
    }
}

#[derive(Resource)]
struct MaskPipeline {
    sun: BindGroupLayoutDescriptor,
    sun_contact: BindGroupLayoutDescriptor,
    layout: BindGroupLayoutDescriptor,
    pipelines: HashMap<MaskKey, CachedComputePipelineId>,
}

/// What the lit pipelines' sun code is specialized on, as Bevy's mesh pipeline names it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MaskKey {
    filter: &'static str,
    contact: bool,
    softness: u8,
}

impl MaskKey {
    fn for_view(filter: Option<&ShadowFilteringMethod>, contact: bool, softness: u8) -> Self {
        let filter = match filter.copied().unwrap_or_default() {
            ShadowFilteringMethod::Hardware2x2 => "SHADOW_FILTER_METHOD_HARDWARE_2X2",
            ShadowFilteringMethod::Gaussian => "SHADOW_FILTER_METHOD_GAUSSIAN",
            ShadowFilteringMethod::Temporal => "SHADOW_FILTER_METHOD_TEMPORAL",
        };
        Self { filter, contact, softness }
    }

    fn softness_def(self) -> Option<&'static str> {
        match self.softness {
            0 => None,
            1 => Some("SHADOW_SOFT"),
            2 => Some("SHADOW_SOFTER"),
            _ => Some("SHADOW_CONTACT_HARDENING"),
        }
    }
}

// Bevy makes the sun's cascade view anew each frame, so the sun's group is remade each frame and kept apart.
#[derive(Default)]
struct MaskGroups {
    sun: Option<(([TextureViewId; 2], [BufferId; 2], Option<BufferId>), BindGroup)>,
    textures: Option<([TextureViewId; 6], BindGroup)>,
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<ShadowMask>();
    // After the camera's size is set this frame, so a resized frame's mask is as big as its prepass.
    app.add_systems(PostUpdate, (size_mask.after(CameraUpdateSystems).in_set(AtlasSizeSystems), follow_softness));
    follow_atlas::<ShadowMask>(app);
    app.add_plugins(ExtractResourcePlugin::<ShadowMask>::default());
    add_shaders(app, [(MASK_SHADER, shader_file("lamp_mask.wesl"))]);
    let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
    render
        .add_systems(RenderStartup, init_mask_pipeline)
        .add_systems(Render, (queue_mask_pipeline, unmark_mask).in_set(RenderSystems::PrepareResources))
        .add_systems(Core3d, cast_shadows.in_set(BeforeMainPasses).after(per_view_shadow_pass::<LATE_SHADOW_PASS>));
    crate::stage_encoder::build(app);
}

fn size_mask(mut mask: ResMut<ShadowMask>, cameras: Query<(&Camera, Has<NormalPrepass>, &Msaa), With<WorldCamera>>, mut images: ResMut<Assets<Image>>) {
    let size = cameras
        .iter()
        .find_map(|(camera, normals, msaa)| camera.physical_target_size().filter(|_| normals && *msaa == Msaa::Off))
        .filter(|size| size.x > 0 && size.y > 0)
        .unwrap_or(UVec2::ZERO);
    if size == mask.size {
        return;
    }
    mask.size = size;
    mask.image = (size != UVec2::ZERO).then(|| images.add(mask_image(size)));
}

fn follow_softness(settings: Option<Res<Settings>>, mut mask: ResMut<ShadowMask>) {
    let softness = settings.map_or(0, |settings| settings.modern_level(MODERN_SHADOW_FILTER));
    if mask.softness != softness {
        mask.softness = softness;
    }
}

fn mask_image(size: UVec2) -> Image {
    let mut image = Image::new_uninit(Extent3d { width: size.x, height: mask_height(size.y), depth_or_array_layers: 1 }, TextureDimension::D2, TextureFormat::Rgba32Float, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.usage |= TextureUsages::STORAGE_BINDING;
    image
}

fn mask_height(rows: u32) -> u32 {
    rows + SUN_ROWS_FROM + rows.div_ceil(SUN_PIXELS)
}

fn init_mask_pipeline(mut commands: Commands) {
    let layout = BindGroupLayoutDescriptor::new(
        "lamp shadow mask",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, binding_types::texture_2d(TextureSampleType::Depth)),
                (1, binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
                (3, binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
                (4, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (5, binding_types::sampler(SamplerBindingType::Filtering)),
                (6, binding_types::texture_storage_2d(TextureFormat::Rgba32Float, StorageTextureAccess::WriteOnly)),
                (8, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (9, binding_types::sampler(SamplerBindingType::Filtering)),
            ),
        ),
    );
    // Bevy's mesh view bindings at their own numbers, which the sun's code imports; the contact shadows' settings only
    // for a view that has them, as the lit pipelines take them.
    let sun = BindGroupLayoutDescriptor::new(
        "lamp shadow mask sun",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, binding_types::uniform_buffer::<ViewUniform>(true)),
                (1, binding_types::uniform_buffer::<GpuLights>(true)),
                (5, binding_types::texture_2d_array(TextureSampleType::Depth)),
                (6, binding_types::sampler(SamplerBindingType::Comparison)),
                (20, binding_types::texture_2d(TextureSampleType::Depth)),
            ),
        ),
    );
    let sun_contact = BindGroupLayoutDescriptor::new(
        "lamp shadow mask sun",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, binding_types::uniform_buffer::<ViewUniform>(true)),
                (1, binding_types::uniform_buffer::<GpuLights>(true)),
                (5, binding_types::texture_2d_array(TextureSampleType::Depth)),
                (6, binding_types::sampler(SamplerBindingType::Comparison)),
                (16, binding_types::uniform_buffer::<ContactShadowsUniform>(true)),
                (20, binding_types::texture_2d(TextureSampleType::Depth)),
            ),
        ),
    );
    commands.insert_resource(MaskPipeline { sun, sun_contact, layout, pipelines: HashMap::default() });
}

fn queue_mask_pipeline(
    cache: Res<PipelineCache>,
    mut pipeline: ResMut<MaskPipeline>,
    mask: Res<ShadowMask>,
    views: Query<(Option<&ShadowFilteringMethod>, Has<ContactShadows>), With<WorldCamera>>,
) {
    let MaskPipeline { sun, sun_contact, layout, pipelines } = &mut *pipeline;
    // Every filter the settings pick (`modern.rs`), with and without contact shadows, is made before it is needed: a
    // frame without the pass lights the world's surfaces with the sun unshadowed and the sky open. A new softness
    // builds while the last one casts (`cast_shadows`).
    let softness = mask.softness;
    let settings = [ShadowFilteringMethod::Hardware2x2, ShadowFilteringMethod::Gaussian].into_iter().flat_map(|filter| [false, true].map(move |contact| MaskKey::for_view(Some(&filter), contact, softness)));
    if views.is_empty() {
        return;
    }
    for key in settings.chain(views.iter().map(|(filter, contact)| MaskKey::for_view(filter, contact, softness))) {
        pipelines.entry(key).or_insert_with(|| {
            cache.queue_compute_pipeline(ComputePipelineDescriptor {
                label: Some("lamp shadow mask".into()),
                layout: vec![if key.contact { sun_contact.clone() } else { sun.clone() }, layout.clone()],
                shader: MASK_SHADER,
                shader_defs: [Some("LAMP_MASK_PASS"), Some("DEPTH_PREPASS"), Some(key.filter), key.contact.then_some("CONTACT_SHADOWS"), key.softness_def()]
                    .into_iter()
                    .flatten()
                    .map(Into::into)
                    .collect(),
                entry_point: Some("shade".into()),
                ..Default::default()
            })
        });
    }
}

/// Before any pass of the frame: in a frame the pass does not run in, actors and blended surfaces cast their own
/// shadows and the world's surfaces light their lamps and the sun unshadowed.
fn unmark_mask(mask: Res<ShadowMask>, images: Res<RenderAssets<GpuImage>>, queue: Res<RenderQueue>) {
    let Some(image) = mask.image.as_ref().and_then(|image| images.get(image)) else { return };
    write_rows(&queue, image, mask.size.y, &[[0.0; 4]]);
}

/// The pipeline for `key`, or while it builds one of another softness with the same filter.
fn ready_pipeline<'a>(pipeline: &MaskPipeline, cache: &'a PipelineCache, key: MaskKey) -> Option<&'a ComputePipeline> {
    let ready = |key: &MaskKey| pipeline.pipelines.get(key).and_then(|id| cache.get_compute_pipeline(*id));
    ready(&key).or_else(|| (0..=3).map(|softness| MaskKey { softness, ..key }).find_map(|key| ready(&key)))
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn cast_shadows(
    view: ViewQuery<
        (&ViewPrepassTextures, &ViewUniformOffset, &ViewLightsUniformOffset, &ViewShadowBindings, Option<&ViewContactShadowsUniformOffset>, Option<&ShadowFilteringMethod>),
        With<WorldCamera>,
    >,
    cache: Res<PipelineCache>,
    pipeline: Res<MaskPipeline>,
    (view_uniforms, light_meta, shadow_samplers, contact_buffer): (Res<ViewUniforms>, Res<LightMeta>, Res<ShadowSamplers>, Res<ContactShadowsBuffer>),
    (atlas, sun_levels, fallback): (Res<LampAtlas>, Res<SunLevelAtlas>, Res<FallbackImage>),
    mask: Res<ShadowMask>,
    images: Res<RenderAssets<GpuImage>>,
    mut groups: Local<MaskGroups>,
    mut stage: ResMut<StageEncoder<BeforeMain>>,
    ctx: RenderContext,
) {
    let (prepass, offset, lights_offset, shadow_bindings, contact_offset, filter) = view.into_inner();
    let contact = contact_offset.zip(contact_buffer.0.binding()).zip(contact_buffer.0.buffer());
    let (Some(compute), Some(depth), Some(normals), Some(uniform), Some(uniforms), Some(lights), Some(lights_buffer), Some(target)) = (
        ready_pipeline(&pipeline, &cache, MaskKey::for_view(filter, contact.is_some(), mask.softness)),
        prepass.depth_only_view(),
        prepass.normal_view(),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer(),
        light_meta.view_gpu_lights.binding(),
        light_meta.view_gpu_lights.buffer(),
        mask.image.as_ref().and_then(|image| images.get(image)),
    ) else {
        return;
    };
    let size = prepass.size;
    let multisampled = prepass.depth.as_ref().is_some_and(|depth| depth.texture.texture.sample_count() > 1);
    if multisampled || target.texture_descriptor.size.width != size.width || target.texture_descriptor.size.height != mask_height(size.height) {
        return;
    }
    // Without lamps or their bake the pass still casts the sun; its lamp half then finds no atlas and holds none.
    let book = atlas.book.as_ref().and_then(|book| images.get(book)).unwrap_or(&fallback.d2);
    let shadows = atlas.wanted().and_then(|image| images.get(&image)).unwrap_or(&fallback.d2);
    let levels = sun_levels.image.as_ref().and_then(|image| images.get(image)).unwrap_or(&fallback.d2);
    let cascades = &shadow_bindings.directional_light_depth_texture_view;
    let sun_ids = ([depth.id(), cascades.id()], [uniforms.id(), lights_buffer.id()], contact.as_ref().map(|(_, buffer)| buffer.id()));
    let offsets = [offset.offset, lights_offset.offset, contact.as_ref().map_or(0, |((at, _), _)| at.0)];
    let offsets = &offsets[..2 + usize::from(contact.is_some())];
    if groups.sun.as_ref().is_none_or(|(held, _)| *held != sun_ids) {
        let device = ctx.render_device();
        let comparison = &shadow_samplers.directional_light_comparison_sampler;
        let made = match contact {
            Some(((_, settings), _)) => device.create_bind_group(
                "lamp shadow mask sun",
                &cache.get_bind_group_layout(&pipeline.sun_contact),
                &BindGroupEntries::with_indices(((0, uniform), (1, lights), (5, cascades), (6, comparison), (16, settings), (20, depth))),
            ),
            None => device.create_bind_group(
                "lamp shadow mask sun",
                &cache.get_bind_group_layout(&pipeline.sun),
                &BindGroupEntries::with_indices(((0, uniform), (1, lights), (5, cascades), (6, comparison), (20, depth))),
            ),
        };
        groups.sun = Some((sun_ids, made));
    }
    let texture_ids = [depth.id(), normals.id(), book.texture_view.id(), shadows.texture_view.id(), target.texture_view.id(), levels.texture_view.id()];
    if groups.textures.as_ref().is_none_or(|(held, _)| *held != texture_ids) {
        let made = ctx.render_device().create_bind_group(
            "lamp shadow mask",
            &cache.get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::with_indices((
                (0, depth),
                (1, normals),
                (3, &book.texture_view),
                (4, &shadows.texture_view),
                (5, &shadows.sampler),
                (6, &target.texture_view),
                (8, &levels.texture_view),
                (9, &levels.sampler),
            )),
        );
        groups.textures = Some((texture_ids, made));
    }
    let (Some((_, sun)), Some((_, textures))) = (&groups.sun, &groups.textures) else { return };
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let encoder = stage.encoder(ctx.render_device());
    let span = diagnostics.time_span(encoder, "lamp_shadow_mask");
    {
        let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor { label: Some("lamp shadow mask"), timestamp_writes: None });
        pass.set_pipeline(compute);
        pass.set_bind_group(0, sun, offsets);
        pass.set_bind_group(1, textures, &[]);
        pass.dispatch_workgroups(size.width.div_ceil(GROUP_SIDE), size.height.div_ceil(GROUP_SIDE), 1);
    }
    span.end(encoder);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_pass_dispatches_the_groups_the_shader_declares() {
        assert_eq!(shader_const(include_str!("../lamp_mask.wesl"), "GROUP_SIDE"), f64::from(GROUP_SIDE));
    }

    #[test]
    fn a_texel_holds_its_lamps_in_three_bytes_a_channel() {
        let lit = include_str!("../lamp_shadow.wesl");
        let (lamps, slots, check) = (shader_const(lit, "SCREEN_LIST_LAMPS"), shader_const(lit, "SCREEN_SLOTS"), shader_const(lit, "SCREEN_CHECK_BYTE"));
        assert!(8.0 + lamps <= 24.0, "the list check's byte and a bit a lamp fit one channel");
        assert_eq!(check, 2.0 * slots, "the check byte follows the slots' bytes");
        assert!(check < 9.0, "three channels of three bytes");
    }

    #[test]
    fn the_mask_holds_every_lamp_a_relief_normal_may_face() {
        assert!(shader_const(include_str!("../lamp_mask.wesl"), "FACING_SLACK") >= shader_const(include_str!("../relief.wesl"), "RELIEF_MAX_TILT"));
    }

    #[test]
    fn the_mask_holds_a_header_row_and_the_suns_rows_after_the_pixels_and_takes_the_pass_writes() {
        let image = mask_image(UVec2::new(2560, 1440));
        assert_eq!((image.texture_descriptor.size.width, image.texture_descriptor.size.height), (2560, 1440 + 1 + 360));
        assert!(image.texture_descriptor.usage.contains(TextureUsages::STORAGE_BINDING));
        assert_eq!(mask_height(5), 5 + 1 + 2);
    }

    #[test]
    fn the_suns_rows_sit_where_the_shaders_read_them() {
        let lit = include_str!("../lamp_shadow.wesl");
        assert_eq!(shader_const(lit, "SCREEN_SUN_FROM"), f64::from(SUN_ROWS_FROM));
        assert_eq!(shader_const(lit, "SCREEN_SUN_PIXELS"), f64::from(SUN_PIXELS));
        assert_eq!(SUN_PIXELS, 4, "a channel for each pixel row of a texel");
        assert_eq!(GROUP_SIDE % SUN_PIXELS, 0, "a group's rows fill whole texels");
    }
}
