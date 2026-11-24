//! The liquid stream: particles poured at a feeder's nozzle with the velocity the scripts aim, stepped by one compute
//! pass while any lives. Each pour gets a lane of the pool and fills it in pouring order, so the pass finds a
//! particle's neighbours in the stream beside it.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::Vec3;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor, BufferId, BufferUsages, CachedComputePipelineId,
    ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache, ShaderStages, TextureId, TextureSampleType, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::view::{ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_LIQUIDS, Settings};
use p2_vm::FluidType;

use super::{LIQUID_BURNING, LIQUID_KIND_MASK, LIQUID_PARTICLE_WORDS, LiquidParticles, LiquidPassSystems};
use crate::components::SceneView;
use crate::frame_facts::FrameFacts;
use crate::gpu_particles::{next_unit, ring_writes};
use crate::particle_draw::{ParticleDrawSystems, draw_particles};
use crate::particle_frame::{DrawnParticles, Liquid, LiquidRole, Pour};
use crate::screen_pass::{PassView, ViewBindGroups, add_view_pass, follow_view_pass, modern_level, tag_pass_views};
use crate::shaders::{add_libraries, add_shaders};

const STREAM_SHADER: Handle<Shader> = uuid_handle!("3e8b5d17-9c42-4a6f-b1d0-62f7a9e4c358");
pub(super) const PARTICLE_BYTES: u64 = LIQUID_PARTICLE_WORDS as u64 * 4;
pub(super) const LANE: u32 = 1024;
const LANES_PER_LEVEL: u32 = 8;
const LANE_CAP: u32 = 64;
/// Matches `@workgroup_size` in `liquid_stream.wesl`.
const WORKGROUP: u32 = 64;
/// Matches `LONGEST_LIFE` in `liquid_stream.wesl`, which no kind outlives.
const LONGEST_LIFE: f64 = 6.0;
/// Seconds a lane is still stepped after its last particle died, so both buffers hold it dead before it is dropped.
const LIFE_MARGIN: f64 = 0.5;
/// Particles poured for each one the scripts' emitter spawns, per level.
const RATE_PER_LEVEL: f32 = 0.5;
/// Unreal units a second of scatter added to the aimed velocity.
const JITTER: f32 = 6.0;
/// The radius word of a slot just poured: any live value, the pass gives it its kind's radius.
const NEWBORN: f32 = 1.0;

pub struct LiquidStreamPlugin;

impl Plugin for LiquidStreamPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, [(STREAM_SHADER, Shader::from_wesl(include_str!("../liquid_stream.wesl"), "p2_render/liquid_stream.wesl"))]);
        add_view_pass::<LiquidStream>(app);
        app.init_resource::<Pourer>().add_systems(PostUpdate, pour_liquids.after(draw_particles).after(tag_pass_views).in_set(ParticleDrawSystems));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .add_systems(RenderStartup, init_stream_pipeline)
            .add_systems(Render, prepare_stream.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, step_liquids.in_set(LiquidPassSystems::Step));
    }
}

fn lane_count(level: u8) -> u32 {
    (LANES_PER_LEVEL * u32::from(level)).min(LANE_CAP)
}

pub(super) fn liquid_bits(kind: FluidType, burning: bool) -> u32 {
    (kind as u32 & LIQUID_KIND_MASK) | if burning { LIQUID_BURNING } else { 0 }
}

/// A particle poured `lead` seconds ago: where it is now, in Bevy axes from the world's origin, and how fast it goes.
fn poured(pour: &Pour, origin: Vec3, lead: f32, bits: u32, seed: &mut u32) -> [u32; LIQUID_PARTICLE_WORDS] {
    let [low, high] = pour.velocity.map(|end| Vec3::from(to_bevy(end)));
    let mut unit = || next_unit(seed);
    let share = Vec3::new(unit(), unit(), unit());
    let scatter = Vec3::new(unit(), unit(), unit()) * 2.0 - Vec3::ONE;
    let velocity = low + (high - low) * share + scatter * JITTER;
    let at = Vec3::from(to_bevy(pour.from)) + origin + velocity * lead;
    let [x, y, z, radius, vx, vy, vz] = [at.x, at.y, at.z, NEWBORN, velocity.x, velocity.y, velocity.z].map(f32::to_bits);
    [x, y, z, radius, vx, vy, vz, bits]
}

#[derive(Clone, Copy)]
struct Lane {
    owner: Entity,
    cursor: u32,
    /// The part of a particle the pour still owes from earlier frames.
    owed: f32,
    gravity: Vec3,
    /// The pour clock past which every particle of the lane is dead.
    quiet_at: f64,
}

impl Lane {
    const FREE: Self = Self { owner: Entity::PLACEHOLDER, cursor: 0, owed: 0.0, gravity: Vec3::ZERO, quiet_at: f64::NEG_INFINITY };
}

#[derive(Resource, Default)]
struct Pourer {
    map: u32,
    /// Seconds poured; a pause stops it.
    clock: f64,
    lanes: Vec<Lane>,
    seed: u32,
}

impl Pourer {
    /// A pour holds its lane while it flows, even on frames that owe less than one particle.
    fn lane_of(&mut self, parent: Entity) -> Option<usize> {
        let clock = self.clock;
        let index = match self.lanes.iter().position(|lane| lane.owner == parent) {
            Some(index) => index,
            None => {
                let index = self.lanes.iter().position(|lane| clock > lane.quiet_at)?;
                self.lanes[index] = Lane { owner: parent, ..Lane::FREE };
                index
            }
        };
        let lane = &mut self.lanes[index];
        lane.quiet_at = lane.quiet_at.max(clock + LIFE_MARGIN);
        Some(index)
    }

    /// The frame's stream, `None` while no particle lives; nothing is poured while `origin` is `None`, out of play.
    fn pour(&mut self, drawn: &DrawnParticles, level: u8, origin: Option<Vec3>, seconds: f32) -> Option<LiquidStream> {
        let lanes = lane_count(level) as usize;
        if lanes == 0 {
            self.lanes.clear();
            return None;
        }
        if self.map != drawn.level || self.lanes.len() != lanes {
            (self.map, self.clock) = (drawn.level, 0.0);
            self.lanes = vec![Lane::FREE; lanes];
        }
        self.clock += f64::from(seconds);
        let (mut newborn, mut runs) = (Vec::new(), Vec::new());
        for actor in drawn.actors.iter().filter(|actor| actor.drawn) {
            let Some(Liquid { kind, role: LiquidRole::Stream | LiquidRole::Drip, burning, pour: Some(pour) }) = actor.liquid else { continue };
            let Some(origin) = origin.filter(|_| pour.flowing) else { continue };
            let Some(index) = self.lane_of(actor.parent) else { continue };
            let lane = &mut self.lanes[index];
            // A lane holds a pour's whole life: faster pouring would write over the tail still in the air.
            let rate = (pour.rate.max(0.0) * RATE_PER_LEVEL * f32::from(level)).min(LANE as f32 / LONGEST_LIFE as f32);
            let owed = lane.owed + rate * seconds;
            let count = owed.floor();
            (lane.owed, lane.gravity) = (owed - count, Vec3::from(to_bevy(pour.acceleration)));
            let count = count as usize;
            if count == 0 {
                continue;
            }
            lane.quiet_at = self.clock + LONGEST_LIFE + LIFE_MARGIN;
            let (bits, from) = (liquid_bits(kind, burning), newborn.len());
            // Spread over the frame, oldest farthest from the nozzle, so one frame's particles string out along the pour.
            newborn.extend((0..count).map(|made| poured(&pour, origin, seconds * (count - made) as f32 / count as f32, bits, &mut self.seed)));
            let (writes, cursor) = ring_writes(lane.cursor, LANE, count);
            lane.cursor = cursor;
            let base = index as u32 * LANE;
            runs.extend(writes.into_iter().map(|write| (base + write.slot, from + write.from, write.len)));
        }
        let last = self.lanes.iter().rposition(|lane| self.clock <= lane.quiet_at)?;
        Some(LiquidStream {
            capacity: lanes as u32 * LANE,
            map: self.map,
            seconds,
            live: (last as u32 + 1) * LANE,
            newborn,
            runs,
            gravity: self.lanes.iter().map(|lane| lane.gravity.extend(0.0).to_array()).collect(),
        })
    }
}

/// On the one scene view whose depth prepass the stream lands on, while any liquid particle lives.
#[derive(Component, Debug, Clone, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
pub struct LiquidStream {
    capacity: u32,
    map: u32,
    seconds: f32,
    /// Slots from the pool's start that are stepped: every lane up to the last that may hold a live particle.
    live: u32,
    newborn: Vec<[u32; LIQUID_PARTICLE_WORDS]>,
    /// Runs of `newborn` as the pool's first slot, the first newborn and how many.
    runs: Vec<(u32, usize, usize)>,
    /// Each lane's gravity, in Bevy axes and Unreal units a second squared.
    gravity: Vec<[f32; 4]>,
}

fn pour_liquids(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    time: Res<Time>,
    facts: Res<FrameFacts>,
    drawn: Option<Res<DrawnParticles>>,
    views: Query<(Entity, Has<PassView>, Option<&LiquidStream>), With<SceneView>>,
    mut pourer: ResMut<Pourer>,
) {
    let stepper = views.iter().find(|&(_, drawn_in, _)| drawn_in).map(|(entity, ..)| entity);
    let seconds = if facts.paused { 0.0 } else { time.delta_secs().min(0.1) };
    let level = modern_level(settings.as_deref(), MODERN_LIQUIDS);
    let mut stream = match (drawn.as_deref(), stepper) {
        (Some(drawn), Some(_)) => pourer.pour(drawn, level, facts.in_play.then_some(facts.origin), seconds),
        _ => {
            pourer.lanes.clear();
            None
        }
    };
    for (entity, _, shown) in &views {
        let wanted = if stepper == Some(entity) { stream.take() } else { None };
        follow_view_pass(commands.entity(entity), shown, wanted);
    }
}

/// `Frame` in `liquid_stream.wesl`.
fn frame_bytes(seconds: f32, live: u32) -> Vec<u8> {
    [seconds.to_bits(), live, LANE, 0].into_iter().flat_map(u32::to_le_bytes).collect()
}

#[derive(Resource)]
struct StreamPipeline {
    layout: BindGroupLayoutDescriptor,
    /// Queued when a stream first pours, so nothing is built while the feature is off.
    step: Option<CachedComputePipelineId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    First,
    Second,
}

/// The pool in two buffers: the pass reads `read`, last frame's state, and writes `write`, which the surface draws.
/// Made zeroed when the level or the map changes, dropped when no particle lives.
#[derive(Resource)]
struct StreamPool {
    read: Buffer,
    write: Buffer,
    lanes: Buffer,
    frame: Buffer,
    capacity: u32,
    map: u32,
    /// Which way round the two buffers are this frame, for their bind group.
    side: Side,
}

impl StreamPool {
    fn new(device: &RenderDevice, stream: &LiquidStream) -> Self {
        let buffer = |label: &'static str, size: u64, usage: BufferUsages| {
            device.create_buffer(&BufferDescriptor { label: Some(label), size, usage: usage | BufferUsages::COPY_DST, mapped_at_creation: false })
        };
        let particles = u64::from(stream.capacity) * PARTICLE_BYTES;
        Self {
            read: buffer("liquid particles", particles, BufferUsages::STORAGE),
            write: buffer("liquid particles", particles, BufferUsages::STORAGE),
            lanes: buffer("liquid lanes", (stream.gravity.len() as u64 * 16).max(16), BufferUsages::STORAGE),
            frame: buffer("liquid frame", 16, BufferUsages::UNIFORM),
            capacity: stream.capacity,
            map: stream.map,
            side: Side::First,
        }
    }
}

fn init_stream_pipeline(mut commands: Commands) {
    let layout = BindGroupLayoutDescriptor::new(
        "liquid stream",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                binding_types::storage_buffer_read_only_sized(false, None),
                binding_types::storage_buffer_sized(false, None),
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::uniform_buffer::<ViewUniform>(true),
                binding_types::uniform_buffer_sized(false, None),
                binding_types::storage_buffer_read_only_sized(false, None),
            ),
        ),
    );
    commands.insert_resource(StreamPipeline { layout, step: None });
}

fn prepare_stream(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipeline: ResMut<StreamPipeline>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    streams: Query<&LiquidStream>,
    pool: Option<ResMut<StreamPool>>,
) {
    let Some(stream) = streams.iter().next() else {
        if pool.is_some() {
            commands.remove_resource::<StreamPool>();
            commands.remove_resource::<LiquidParticles>();
        }
        return;
    };
    if pipeline.step.is_none() {
        pipeline.step = Some(cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("liquid stream".into()),
            layout: vec![pipeline.layout.clone()],
            shader: STREAM_SHADER,
            entry_point: Some("flow".into()),
            ..Default::default()
        }));
    }
    if pipeline.step.and_then(|id| cache.get_compute_pipeline(id)).is_none() {
        commands.remove_resource::<LiquidParticles>();
        return;
    }
    let Some(pool) = pool.filter(|pool| pool.capacity == stream.capacity && pool.map == stream.map) else {
        commands.insert_resource(StreamPool::new(&device, stream));
        commands.remove_resource::<LiquidParticles>();
        return;
    };
    let pool = pool.into_inner();
    std::mem::swap(&mut pool.read, &mut pool.write);
    pool.side = if pool.side == Side::First { Side::Second } else { Side::First };
    let bytes: Vec<u8> = stream.newborn.iter().flatten().flat_map(|word| word.to_le_bytes()).collect();
    let stride = PARTICLE_BYTES as usize;
    for &(slot, from, len) in &stream.runs {
        if let Some(range) = bytes.get(from * stride..(from + len) * stride) {
            queue.write_buffer(&pool.read, u64::from(slot) * PARTICLE_BYTES, range);
        }
    }
    let gravity: Vec<u8> = stream.gravity.iter().flatten().flat_map(|word| word.to_le_bytes()).collect();
    queue.write_buffer(&pool.lanes, 0, &gravity);
    queue.write_buffer(&pool.frame, 0, &frame_bytes(stream.seconds, stream.live));
    commands.insert_resource(LiquidParticles { buffer: pool.write.clone(), capacity: pool.capacity, live: stream.live });
}

fn step_liquids(
    view: ViewQuery<(&ViewPrepassTextures, &ViewUniformOffset, &LiquidStream)>,
    cache: Res<PipelineCache>,
    pipeline: Res<StreamPipeline>,
    pool: Option<Res<StreamPool>>,
    view_uniforms: Res<ViewUniforms>,
    mut groups: Local<[ViewBindGroups<(BufferId, BufferId, TextureId, BufferId, BufferId, BufferId)>; 2]>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (prepass, offset, stream) = view.into_inner();
    let (Some(pool), Some(step), Some(depth), Some(depth_texture), Some(uniform), Some(uniforms)) = (
        pool,
        pipeline.step.and_then(|id| cache.get_compute_pipeline(id)),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer().map(|uniforms| uniforms.id()),
    ) else {
        return;
    };
    let [first, second] = &mut *groups;
    let groups = if pool.side == Side::First { first } else { second };
    let key = (pool.read.id(), pool.write.id(), depth_texture, uniforms, pool.lanes.id(), pool.frame.id());
    let device = ctx.render_device();
    let group = groups.get_or_make(entity, key, || {
        device.create_bind_group(
            "liquid stream",
            &cache.get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::sequential((
                pool.read.as_entire_binding(),
                pool.write.as_entire_binding(),
                depth,
                uniform,
                pool.frame.as_entire_binding(),
                pool.lanes.as_entire_binding(),
            )),
        )
    });
    let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor { label: Some("liquid stream"), timestamp_writes: None });
    pass.set_pipeline(step);
    pass.set_bind_group(0, group, &[offset.offset]);
    pass.dispatch_workgroups(stream.live.div_ceil(WORKGROUP), 1, 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquids::LIQUID_LANDED;
    use crate::particle_frame::DrawnActor;
    use crate::shaders::shader_const;
    use p2_math::Vector;

    const SHADER: &str = include_str!("../liquid_stream.wesl");

    #[test]
    fn the_shader_holds_the_same_bits_and_longest_life() {
        assert_eq!(shader_const(SHADER, "KIND_MASK"), f64::from(LIQUID_KIND_MASK));
        assert_eq!(shader_const(SHADER, "LANDED"), f64::from(LIQUID_LANDED));
        assert_eq!(shader_const(SHADER, "BURNING"), f64::from(LIQUID_BURNING));
        assert_eq!(shader_const(SHADER, "LONGEST_LIFE"), LONGEST_LIFE);
        assert_eq!(PARTICLE_BYTES, 32);
    }

    #[test]
    fn the_pool_grows_with_the_level_and_stops_at_zero() {
        assert_eq!(lane_count(0), 0);
        assert_eq!(lane_count(1) * LANE, 8192);
        assert_eq!(lane_count(6), 6 * LANES_PER_LEVEL);
        assert_eq!(lane_count(10), LANE_CAP);
        let mut pourer = Pourer::default();
        let drawn = drawn_with(vec![pouring(Entity::PLACEHOLDER, 1000.0)]);
        assert_eq!(pourer.pour(&drawn, 0, Some(Vec3::ZERO), 0.1), None, "level 0 pours nothing");
        assert!(pourer.lanes.is_empty());
    }

    fn pour(rate: f32) -> Pour {
        Pour {
            from: Vector::new(0.0, 0.0, 100.0),
            velocity: [Vector::new(100.0, 0.0, 0.0), Vector::new(200.0, 0.0, 50.0)],
            acceleration: Vector::new(0.0, 0.0, -800.0),
            rate,
            flowing: true,
        }
    }

    fn pouring(parent: Entity, rate: f32) -> DrawnActor {
        let liquid = Liquid { kind: FluidType::Urine, role: LiquidRole::Stream, burning: false, pour: Some(pour(rate)) };
        DrawnActor { parent, drawn: true, liquid: Some(liquid), ..DrawnActor::default() }
    }

    fn drawn_with(actors: Vec<DrawnActor>) -> DrawnParticles {
        DrawnParticles { actors, ..DrawnParticles::default() }
    }

    #[test]
    fn a_slow_pour_carries_its_fraction_from_frame_to_frame() {
        let mut pourer = Pourer::default();
        let drawn = drawn_with(vec![pouring(Entity::PLACEHOLDER, 2.0)]);
        let poured: usize = (0..12).filter_map(|_| pourer.pour(&drawn, 2, Some(Vec3::ZERO), 0.125)).map(|stream| stream.newborn.len()).sum();
        assert_eq!(poured, 3, "level 2 pours the scripts' rate");
        let stream = pourer.pour(&drawn, 2, Some(Vec3::ZERO), 1.0).unwrap();
        assert_eq!(stream.newborn.len(), 2);
        assert_eq!(stream.runs, vec![(3, 0, 2)]);
        assert_eq!(stream.live, LANE);
        assert_eq!(stream.gravity[0], [0.0, -800.0, 0.0, 0.0], "gravity in Bevy axes");
    }

    #[test]
    fn only_a_flowing_drawn_stream_or_drip_pours() {
        let mut pourer = Pourer::default();
        let mut stopped = pouring(Entity::PLACEHOLDER, 100.0);
        if let Some(Liquid { pour: Some(pour), .. }) = &mut stopped.liquid {
            pour.flowing = false;
        }
        let mut puddle = pouring(Entity::PLACEHOLDER, 100.0);
        if let Some(liquid) = &mut puddle.liquid {
            liquid.role = LiquidRole::Puddle;
        }
        let hidden = DrawnActor { drawn: false, ..pouring(Entity::PLACEHOLDER, 100.0) };
        for actor in [stopped, puddle, hidden] {
            assert_eq!(pourer.pour(&drawn_with(vec![actor]), 5, Some(Vec3::ZERO), 0.5), None);
        }
        assert_eq!(pourer.pour(&drawn_with(vec![pouring(Entity::PLACEHOLDER, 100.0)]), 5, None, 0.5), None, "nothing is poured out of play");
    }

    #[test]
    fn two_pours_fill_their_own_lanes_and_the_pool_goes_quiet() {
        let mut world = bevy::ecs::world::World::new();
        let (a, b) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut pourer = Pourer::default();
        let stream = pourer.pour(&drawn_with(vec![pouring(a, 20.0), pouring(b, 20.0)]), 2, Some(Vec3::ZERO), 0.5).unwrap();
        assert_eq!(stream.runs, vec![(0, 0, 10), (LANE, 10, 10)]);
        assert_eq!(stream.live, 2 * LANE);
        assert_eq!(stream.capacity, lane_count(2) * LANE);
        let quiet = drawn_with(Vec::new());
        assert!(pourer.pour(&quiet, 2, Some(Vec3::ZERO), 0.0).is_some(), "a pause does not run the clock out");
        assert!(pourer.pour(&quiet, 2, Some(Vec3::ZERO), LONGEST_LIFE as f32).is_some(), "the last particles still live");
        assert_eq!(pourer.pour(&quiet, 2, Some(Vec3::ZERO), LIFE_MARGIN as f32 + 0.01), None);
        let again = pourer.pour(&drawn_with(vec![pouring(b, 20.0)]), 2, Some(Vec3::ZERO), 0.5).unwrap();
        assert_eq!(again.runs, vec![(LANE + 10, 0, 10)], "a pour keeps its lane and goes on around it");
    }

    #[test]
    fn a_particle_packs_its_place_speed_and_kind() {
        assert_eq!(liquid_bits(FluidType::Napalm, true), 8 | LIQUID_BURNING);
        assert_eq!(liquid_bits(FluidType::Blood, false), 3);
        let mut seed = 7;
        for _ in 0..100 {
            let words = poured(&pour(1.0), Vec3::new(10.0, 0.0, 0.0), 0.0, 3, &mut seed);
            let [x, y, z, radius, vx, vy, vz] = [0, 1, 2, 3, 4, 5, 6].map(|word| f32::from_bits(words[word]));
            assert_eq!([x, y, z, radius], [10.0, 100.0, 0.0, NEWBORN], "the nozzle in Bevy axes from the world's origin");
            assert!((100.0 - JITTER..=200.0 + JITTER).contains(&vx), "{vx}");
            assert!((-JITTER..=50.0 + JITTER).contains(&vy), "Unreal up is Bevy y: {vy}");
            assert!(vz.abs() <= JITTER);
            assert_eq!(words[7], 3);
        }
        let ahead = poured(&pour(1.0), Vec3::ZERO, 0.1, 3, &mut seed);
        assert!(f32::from_bits(ahead[0]) > 9.0, "a particle poured earlier in the frame has flown on");
    }

    #[test]
    fn the_frame_uniform_holds_the_step_and_the_lane() {
        let bytes = frame_bytes(0.25, 512);
        assert_eq!(bytes.len(), 16);
        assert_eq!(bytes[..4], 0.25f32.to_le_bytes());
        assert_eq!(bytes[4..8], 512u32.to_le_bytes());
        assert_eq!(bytes[8..12], LANE.to_le_bytes());
    }
}
