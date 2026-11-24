//! The liquid layer on the GPU: `liquid_layer.wesl` steps every live tile and publishes it into the atlas, in one
//! compute pass that goes out with the frame's submit, ahead of the cameras' passes that read the atlas.

use bevy::app::{App, Plugin};
use bevy::asset::{Handle, uuid_handle};
use bevy::ecs::prelude::{Commands, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::Vec2;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor, BufferId, BufferUsages, CachedComputePipelineId,
    ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache, ShaderStages, ShaderType, StorageTextureAccess,
    TextureFormat, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

use super::layer::{FILM, LayerFrame, SIDE, SLOT_SIDE, STEP, TEXEL, TEXELS, WALL};
use crate::screen_pass::shader_bytes;
use crate::shaders::{add_shaders, shader_file};
use crate::stage_encoder::{Prepared, StageEncoder};

const LAYER_SHADER: Handle<Shader> = uuid_handle!("3f6b2d84-9a1c-4e57-b0d3-8c2e5f71a946");
/// Matches `@workgroup_size` in `liquid_layer.wesl`.
const WORKGROUP: u32 = 8;
const GRAVITY: f32 = 950.0;
/// What of a pipe's flow is kept from one step to the next: the liquid's thickness.
const DAMPING: f32 = 0.9;
const WATER_BYTES: u64 = 8;
const FLUX_BYTES: u64 = 8;
const FLOOR_BYTES: u64 = 4;

/// `Step` in `liquid_layer.wesl`.
#[derive(ShaderType)]
struct LayerStep {
    seconds: f32,
    film: f32,
    drying: f32,
    pull: f32,
    damping: f32,
    seed: u32,
    pad: Vec2,
}

pub struct LayerGpuPlugin;

impl Plugin for LayerGpuPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        crate::stage_encoder::build(app);
        add_shaders(app, [(LAYER_SHADER, shader_file("liquid_layer.wesl"))]);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<LayerState>().add_systems(RenderStartup, init_layer).add_systems(Render, step_layer.in_set(RenderSystems::PrepareResources));
    }
}

#[derive(Resource)]
struct LayerPipelines {
    layout: BindGroupLayoutDescriptor,
    /// `hold`, `pipes`, `level` and `publish`, queued when liquid first lives, so nothing is built while it is off.
    entries: Option<[CachedComputePipelineId; 4]>,
}

struct LayerBuffers {
    capacity: u32,
    level: u32,
    water: [Buffer; 2],
    flux: Buffer,
    floors: Buffer,
    step: Buffer,
    slots: Buffer,
    active: Buffer,
    sources: Buffer,
    current: usize,
    frames: u32,
    groups: Option<([BindGroup; 2], [BufferId; 3], TextureViewId)>,
}

#[derive(Resource, Default)]
struct LayerState(Option<LayerBuffers>);

fn init_layer(mut commands: Commands) {
    let layout = BindGroupLayoutDescriptor::new(
        "liquid layer",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                binding_types::uniform_buffer::<LayerStep>(false),
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::storage_buffer_sized(false, None),
                binding_types::storage_buffer_sized(false, None),
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::texture_storage_2d(TextureFormat::Rgba16Float, StorageTextureAccess::WriteOnly),
            ),
        ),
    );
    commands.insert_resource(LayerPipelines { layout, entries: None });
}

fn buffer(device: &RenderDevice, label: &'static str, size: u64, usage: BufferUsages) -> Buffer {
    device.create_buffer(&BufferDescriptor { label: Some(label), size: size.max(16), usage: usage | BufferUsages::COPY_DST, mapped_at_creation: false })
}

fn new_buffers(device: &RenderDevice, capacity: u32, level: u32) -> LayerBuffers {
    let texels = u64::from(capacity) * TEXELS as u64;
    let storage = BufferUsages::STORAGE;
    LayerBuffers {
        capacity,
        level,
        water: [buffer(device, "liquid water a", texels * WATER_BYTES, storage), buffer(device, "liquid water b", texels * WATER_BYTES, storage)],
        flux: buffer(device, "liquid flux", texels * FLUX_BYTES, storage),
        floors: buffer(device, "liquid floors", texels * FLOOR_BYTES, storage),
        step: buffer(device, "liquid step", LayerStep::min_size().get(), BufferUsages::UNIFORM),
        slots: buffer(device, "liquid slots", 0, storage),
        active: buffer(device, "liquid active", 0, storage),
        sources: buffer(device, "liquid sources", 0, storage),
        current: 0,
        frames: 0,
        groups: None,
    }
}

fn fill(device: &RenderDevice, queue: &RenderQueue, buffer: &mut Buffer, label: &'static str, bytes: &[u8]) {
    if bytes.len() as u64 > buffer.size() {
        *buffer = self::buffer(device, label, (bytes.len() as u64).next_power_of_two(), BufferUsages::STORAGE);
    }
    queue.write_buffer(buffer, 0, bytes);
}

fn floor_bytes(floors: &[f32]) -> Vec<u8> {
    floors.iter().flat_map(|z| z.to_le_bytes()).collect()
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn step_layer(
    frame: Option<Res<LayerFrame>>,
    mut pipelines: ResMut<LayerPipelines>,
    mut state: ResMut<LayerState>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    images: Res<RenderAssets<GpuImage>>,
    mut stage: ResMut<StageEncoder<Prepared>>,
) {
    let Some(frame) = frame.filter(|frame| frame.capacity > 0 && !frame.active.is_empty()) else { return };
    if pipelines.entries.is_none() {
        let layout = pipelines.layout.clone();
        pipelines.entries = Some(["hold", "pipes", "level", "publish"].map(|entry| {
            cache.queue_compute_pipeline(ComputePipelineDescriptor {
                label: Some(format!("liquid layer {entry}").into()),
                layout: vec![layout.clone()],
                shader: LAYER_SHADER,
                entry_point: Some(entry.into()),
                ..Default::default()
            })
        }));
    }
    let buffers = match &mut state.0 {
        Some(buffers) if buffers.capacity == frame.capacity && buffers.level == frame.level => buffers,
        stale => stale.insert(new_buffers(&device, frame.capacity, frame.level)),
    };
    let encoder = stage.encoder(&device);
    if !frame.cleared.is_empty() {
        let walls = floor_bytes(&vec![WALL; TEXELS]);
        for &slot in &frame.cleared {
            let at = u64::from(slot) * TEXELS as u64;
            for water in &buffers.water {
                encoder.clear_buffer(water, at * WATER_BYTES, Some(TEXELS as u64 * WATER_BYTES));
            }
            encoder.clear_buffer(&buffers.flux, at * FLUX_BYTES, Some(TEXELS as u64 * FLUX_BYTES));
            queue.write_buffer(&buffers.floors, at * FLOOR_BYTES, &walls);
        }
    }
    for (slot, floors) in &frame.floors {
        queue.write_buffer(&buffers.floors, u64::from(*slot) * TEXELS as u64 * FLOOR_BYTES, &floor_bytes(floors));
    }
    buffers.frames = buffers.frames.wrapping_add(1);
    let step = LayerStep { seconds: STEP, film: FILM, drying: FILM / frame.film_life, pull: GRAVITY * TEXEL, damping: DAMPING, seed: buffers.frames, pad: Vec2::ZERO };
    queue.write_buffer(&buffers.step, 0, &shader_bytes(&step));
    fill(&device, &queue, &mut buffers.slots, "liquid slots", &shader_bytes(frame.slots.as_slice()));
    fill(&device, &queue, &mut buffers.active, "liquid active", &shader_bytes(frame.active.as_slice()));
    if !frame.sources.is_empty() {
        fill(&device, &queue, &mut buffers.sources, "liquid sources", &shader_bytes(frame.sources.as_slice()));
    }
    let ready = pipelines.entries.and_then(|entries| {
        let [hold, pipes, level, publish] = entries.map(|id| cache.get_compute_pipeline(id));
        Some([hold?, pipes?, level?, publish?])
    });
    let (Some(atlas), Some([hold, pipes, level, publish])) = (frame.atlas.and_then(|id| images.get(id)), ready) else {
        return;
    };
    let made_with = [buffers.slots.id(), buffers.active.id(), buffers.sources.id()];
    if buffers.groups.as_ref().is_none_or(|(_, with, view)| *with != made_with || *view != atlas.texture_view.id()) {
        let layout = cache.get_bind_group_layout(&pipelines.layout);
        let group = |read: usize| {
            device.create_bind_group(
                "liquid layer",
                &layout,
                &BindGroupEntries::sequential((
                    buffers.step.as_entire_binding(),
                    buffers.slots.as_entire_binding(),
                    buffers.active.as_entire_binding(),
                    buffers.sources.as_entire_binding(),
                    buffers.water[read].as_entire_binding(),
                    buffers.water[1 - read].as_entire_binding(),
                    buffers.flux.as_entire_binding(),
                    buffers.floors.as_entire_binding(),
                    &atlas.texture_view,
                )),
            )
        };
        buffers.groups = Some(([group(0), group(1)], made_with, atlas.texture_view.id()));
    }
    let Some((groups, ..)) = &buffers.groups else { return };
    let tiles = frame.active.len() as u32;
    let (texels, apron) = (SIDE.div_ceil(WORKGROUP), SLOT_SIDE.div_ceil(WORKGROUP));
    let mut current = buffers.current;
    {
        let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor { label: Some("liquid layer"), timestamp_writes: None });
        if !frame.sources.is_empty() {
            // The group that reads the other buffer writes the current one.
            pass.set_bind_group(0, &groups[1 - current], &[]);
            pass.set_pipeline(hold);
            pass.dispatch_workgroups(texels, texels, tiles);
        }
        for _ in 0..frame.steps {
            pass.set_bind_group(0, &groups[current], &[]);
            pass.set_pipeline(pipes);
            pass.dispatch_workgroups(texels, texels, tiles);
            pass.set_pipeline(level);
            pass.dispatch_workgroups(texels, texels, tiles);
            current = 1 - current;
        }
        pass.set_bind_group(0, &groups[current], &[]);
        pass.set_pipeline(publish);
        pass.dispatch_workgroups(apron, apron, tiles);
    }
    buffers.current = current;
}
