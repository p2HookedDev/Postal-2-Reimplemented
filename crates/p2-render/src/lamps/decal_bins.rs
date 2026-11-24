//! The decals that may meet each screen tile of the world camera, cut after its depth prepass into rows of the lamp
//! book, so a pixel walks its tile's list in place of Bevy's cluster list (`decal_bins.wesl`, `projector.wesl`
//! `decal_list`). Bevy lists a decal in every cluster within twice its box's own sphere, and a liquid tile's box is 256
//! units across.

use bevy::app::App;
use bevy::asset::{Handle, uuid_handle};
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::prelude::{Commands, Local, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::pbr::decal::clustered::{DecalsBuffer, RenderClusteredDecals};
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor, BufferId, BufferUsages,
    CachedComputePipelineId, ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache, ShaderStages, StorageTextureAccess, TextureFormat,
    TextureId, TextureSampleType, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::{ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

use super::LampAtlas;
use super::atlas::write_rows;
use super::cells::{BOOK_WIDTH, DECAL_BIN_ROWS, DECAL_BINS_FROM_END};
use crate::shaders::{add_shaders, shader_file};
use crate::stage_encoder::{BeforeMain, BeforeMainPasses, StageEncoder};
use crate::world_pass::WorldCamera;

const BINS_SHADER: Handle<Shader> = uuid_handle!("4c8e2f71-9a3d-4b56-8e10-d7f2a6c95b34");
/// Texels before the tiles' lists: the grid, then two columns of the binned view's matrix. `decal_bins.wesl` and
/// `projector.wesl` hold the same numbers.
const HEAD_TEXELS: u32 = 4;
/// A tile's texels: its count, then up to 63 decal indices, four a texel.
const LIST_TEXELS: u32 = 16;
const TILE_PIXELS: u32 = 16;
const MOST_TILES: u32 = 32_768;
/// The bits of a tile's mask in `decal_bins.wesl`; with more decals every pixel keeps Bevy's cluster list.
const MOST_DECALS: usize = 1024;
const _: () = assert!(HEAD_TEXELS + MOST_TILES * LIST_TEXELS <= DECAL_BIN_ROWS * BOOK_WIDTH as u32);

#[derive(Resource)]
struct BinPipeline {
    layout: BindGroupLayoutDescriptor,
    pipeline: Option<CachedComputePipelineId>,
    params: Buffer,
}

pub(super) fn build(app: &mut App) {
    add_shaders(app, [(BINS_SHADER, shader_file("decal_bins.wesl"))]);
    let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
    render
        .add_systems(RenderStartup, init_bin_pipeline)
        .add_systems(Render, (queue_bin_pipeline, unmark_bins).in_set(RenderSystems::PrepareResources))
        .add_systems(Core3d, bin_decals.in_set(BeforeMainPasses));
    crate::stage_encoder::build(app);
}

fn init_bin_pipeline(mut commands: Commands, device: Res<RenderDevice>) {
    let layout = BindGroupLayoutDescriptor::new(
        "decal bins",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::uniform_buffer::<ViewUniform>(true),
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::texture_storage_2d(TextureFormat::Rgba32Float, StorageTextureAccess::WriteOnly),
                binding_types::uniform_buffer_sized(false, None),
            ),
        ),
    );
    let params = device.create_buffer(&BufferDescriptor { label: Some("decal bins"), size: 32, usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST, mapped_at_creation: false });
    commands.insert_resource(BinPipeline { layout, pipeline: None, params });
}

fn queue_bin_pipeline(cache: Res<PipelineCache>, mut pipeline: ResMut<BinPipeline>) {
    if pipeline.pipeline.is_none() {
        let layout = pipeline.layout.clone();
        pipeline.pipeline = Some(cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("decal bins".into()),
            layout: vec![layout],
            shader: BINS_SHADER,
            entry_point: Some("bin".into()),
            ..Default::default()
        }));
    }
}

/// Before any pass of the frame: a frame the bins are not cut in leaves every pixel on Bevy's cluster list.
fn unmark_bins(atlas: Res<LampAtlas>, images: Res<RenderAssets<GpuImage>>, queue: Res<RenderQueue>) {
    let Some(book) = atlas.book.as_ref().and_then(|book| images.get(book)) else { return };
    let Some(row) = book.texture_descriptor.size.height.checked_sub(DECAL_BINS_FROM_END) else { return };
    write_rows(&queue, book, row, &[[0.0; 4]]);
}

/// The tile's side in pixels that keeps the grid within the book's rows, and the tiles across and down.
fn tile_grid(width: u32, height: u32) -> (u32, u32, u32) {
    let mut side = TILE_PIXELS;
    while width.div_ceil(side) * height.div_ceil(side) > MOST_TILES {
        side *= 2;
    }
    (side, width.div_ceil(side), height.div_ceil(side))
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn bin_decals(
    view: ViewQuery<(&ViewPrepassTextures, &ViewUniformOffset), With<WorldCamera>>,
    cache: Res<PipelineCache>,
    pipeline: Res<BinPipeline>,
    view_uniforms: Res<ViewUniforms>,
    atlas: Res<LampAtlas>,
    images: Res<RenderAssets<GpuImage>>,
    decals: Res<DecalsBuffer>,
    listed: Res<RenderClusteredDecals>,
    queue: Res<RenderQueue>,
    mut group: Local<Option<((TextureId, BufferId, BufferId, TextureViewId), BindGroup)>>,
    mut stage: ResMut<StageEncoder<BeforeMain>>,
    ctx: RenderContext,
) {
    let (prepass, offset) = view.into_inner();
    let count = listed.len();
    // The pass reads a single-sampled depth.
    let multisampled = prepass.depth.as_ref().is_some_and(|depth| depth.texture.texture.sample_count() > 1);
    if count == 0 || count > MOST_DECALS || multisampled {
        return;
    }
    let (Some(compute), Some(depth), Some(depth_texture), Some(uniform), Some(uniforms), Some(decal_buffer), Some(book)) = (
        pipeline.pipeline.and_then(|id| cache.get_compute_pipeline(id)),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer().map(|buffer| buffer.id()),
        decals.buffer(),
        atlas.book.as_ref().and_then(|book| images.get(book)),
    ) else {
        return;
    };
    let Some(row) = book.texture_descriptor.size.height.checked_sub(DECAL_BINS_FROM_END) else { return };
    let (side, across, down) = tile_grid(prepass.size.width, prepass.size.height);
    let lists = row * BOOK_WIDTH as u32 + HEAD_TEXELS;
    let params = [across, down, side, count as u32, lists, 0, 0, 0];
    queue.write_buffer(&pipeline.params, 0, &params.iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<u8>>());
    let key = (depth_texture, uniforms, decal_buffer.id(), book.texture_view.id());
    if group.as_ref().is_none_or(|(held, _)| *held != key) {
        let made = ctx.render_device().create_bind_group(
            "decal bins",
            &cache.get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::sequential((depth, uniform, decal_buffer.as_entire_binding(), &book.texture_view, pipeline.params.as_entire_binding())),
        );
        *group = Some((key, made));
    }
    let Some((_, bind_group)) = group.as_ref() else { return };
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let encoder = stage.encoder(ctx.render_device());
    let span = diagnostics.time_span(encoder, "decal_bins");
    {
        let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor { label: Some("decal bins"), timestamp_writes: None });
        pass.set_pipeline(compute);
        pass.set_bind_group(0, bind_group, &[offset.offset]);
        pass.dispatch_workgroups(across, down, 1);
    }
    span.end(encoder);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_shaders_read_the_bins_as_they_are_written() {
        let bins = include_str!("../decal_bins.wesl");
        let projector = include_str!("../projector.wesl");
        assert_eq!(shader_const(bins, "HEAD_TEXELS"), f64::from(HEAD_TEXELS));
        assert_eq!(shader_const(projector, "BIN_HEAD_TEXELS"), f64::from(HEAD_TEXELS));
        assert_eq!(shader_const(bins, "LIST_TEXELS"), f64::from(LIST_TEXELS));
        assert_eq!(shader_const(projector, "BIN_LIST_TEXELS"), f64::from(LIST_TEXELS));
        assert_eq!(shader_const(bins, "MASK_WORDS"), (MOST_DECALS / 32) as f64);
        assert_eq!(shader_const(bins, "BOOK_WIDTH"), BOOK_WIDTH as f64);
    }

    #[test]
    fn the_tile_grid_stays_in_the_books_rows_at_any_size() {
        assert_eq!(tile_grid(2560, 1440), (16, 160, 90));
        assert_eq!(tile_grid(3840, 2160), (16, 240, 135));
        let (side, across, down) = tile_grid(7680, 4320);
        assert!(across * down <= MOST_TILES && side == 32, "{side} {across} {down}");
    }
}
