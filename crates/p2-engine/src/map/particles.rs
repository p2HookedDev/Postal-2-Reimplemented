use bevy::camera::primitives::MeshAabb;
use bevy::ecs::change_detection::DetectChangesMut;
use p2_math::Vector;
use p2_particles::{CollisionHit, DirectionUsage, EmitterKind, MeshParticle, Owner};

use super::*;
use super::fluids::camera_view;
use super::spawned::{ModelBuild, ModelKey, SpawnModel, actor_image, static_model_key};
use p2_render::view::alpha_key;
use crate::emitter_changes::apply_command;
use p2_render::particle_frame::{DrawnActor, DrawnEmitter, DrawnParticles, EmitterLook, EmitterShape};
use p2_render::level_collision::StaticCollision;
mod mesh_particles;
mod sprites;
mod bounds;
use mesh_particles::{MeshPool, mesh_box, place_mesh_particles};
pub(crate) use mesh_particles::PoolSlot;
use sprites::{fill_draw_buffers, standard_alpha};
use p2_render::particle_draw::bounds::OCCLUSION_MARGIN;
use bounds::{apply_zone, particles_box};

pub struct ParticleReport {
    pub name: String,
    pub distance: f32,
    pub hidden: bool,
    pub unseen: bool,
    pub zone_culled: bool,
    pub overlay: Option<usize>,
    pub emitters: Vec<String>,
}

pub(super) struct ParticleActor {
    actor: ObjectId,
    simulation: Emitter,
    pub(super) parent: Entity,
    draws: Vec<EmitterDraw>,
    objects: Vec<ObjectId>,
    bounds: Option<(Vec3, Vec3)>,
    buffers: DrawBuffers,
    zone_box: Option<[Vec3; 3]>,
    overlay: Option<usize>,
    name: String,
    zone_seen: bool,
    drawn: bool,
    born: std::time::Instant,
    peak: usize,
    frames: ParticleFrames,
    turn: Turn,
    puddle_seed: Option<u32>,
}

/// What one frame's update hands from its part on the main thread to the part worked on several threads and back.
#[derive(Default)]
struct Turn {
    owner: Owner,
    place: Option<Vector>,
    /// The simulation's step, on a frame that holds a game tick.
    step: Option<f32>,
    /// Sprites are drawn this part of the way from their place before the newest tick to their newest (`shown_part`).
    shown: f32,
    hidden: bool,
    here: Vec3,
    seen: bool,
    seen_box: Option<(Vec3, Vec3)>,
}

/// Emitters below this are worked on the calling thread: handing them out costs more than it saves.
const PARALLEL_EMITTERS: usize = 32;

#[derive(Default)]
struct DrawBuffers {
    meshes: Vec<(Vec<MeshParticle>, f32)>,
    strip_order: Vec<usize>,
}

#[derive(Default)]
struct ParticleFrames {
    drawn: u32,
    zone_unseen: u32,
    hidden: u32,
    out_of_view: u32,
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct PoolMaterialKey {
    texture: Option<bevy::asset::AssetId<Image>>,
    alpha: (std::mem::Discriminant<AlphaMode>, u32),
    two_sided: bool,
}

struct EmitterDraw {
    disabled: bool,
    pool: Option<MeshPool>,
    pending_model: Option<ModelKey>,
    name: String,
}

pub(super) fn particle_collision(world: &StaticCollision, start: Vector, end: Vector) -> Option<CollisionHit> {
    let hit = world.first_hit(start, end)?;
    Some(CollisionHit { location: hit.location, normal: hit.normal })
}

#[derive(Resource, Default)]
pub struct Particles {
    pub(super) actors: Vec<ParticleActor>,
    pub(super) particle_emitters: FxHashMap<ObjectId, (usize, usize)>,
    pub(super) pool_materials: HashMap<PoolMaterialKey, Handle<StandardMaterial>>,
    pub(super) waiting_sprites: Vec<SpriteActor>,
    pub(super) waiting_coronas: Vec<CoronaActor>,
    pub(super) born: Vec<EmitterLook>,
}

impl Session {
    pub(super) fn add_particles(&mut self, emitters: Vec<(ObjectId, EmitterActor)>, origin: Vec3, t: &mut SpawnTargets) {
        if emitters.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for (actor, emitter) in emitters {
            let (objects, emitter_meshes, name) = (emitter.objects, emitter.meshes, emitter.name);
            let fogless = emitter.settings.disable_fogging;
            let parent = t.commands.spawn((MapScoped, Transform::from_translation(origin), Visibility::default())).id();
            let mut draws = Vec::new();
            let mut props = Vec::new();
            for (index, ((p, texture), mesh)) in emitter.emitters.into_iter().zip(emitter_meshes).enumerate() {
                let emitter_name = if p.kind == EmitterKind::Mesh { mesh.clone() } else { texture.clone() }.unwrap_or_else(|| "-".into());
                let (mut pool, mut pending_model) = (None, None);
                if p.kind == EmitterKind::Mesh && let Some(path) = mesh {
                    let key = static_model_key(&path, &[]);
                    match t.spawned.spawn_models.get(&key) {
                        Some(Some(SpawnModel::Static { sections, .. })) => pool = Some(MeshPool::new(sections, &p, t.assets, &mut t.particles.pool_materials)),
                        Some(_) => {}
                        None => {
                            if !t.spawned.spawn_builds.iter().any(|build| build.key == key) {
                                let build = ModelBuild::start_static(game, t.assets, key.clone(), path);
                                t.spawned.spawn_builds.push(build);
                            }
                            pending_model = Some(key);
                        }
                    }
                }
                let started = std::time::Instant::now();
                let spawned = &mut *t.spawned;
                let image = texture.filter(|_| p.kind != EmitterKind::Mesh).and_then(|path| actor_image(&spawned.actor_textures, &mut spawned.texture_builds, game, t.assets, &path));
                t.times.add("spawned: emitters: textures", started);
                if let Some(image) = image {
                    t.particles.born.push(EmitterLook { parent, order: index as u32, image, props: p.clone(), name: emitter_name.clone(), fogless });
                }
                draws.push(EmitterDraw { disabled: p.disabled, pool, pending_model, name: emitter_name });
                props.push(p);
            }
            let started = std::time::Instant::now();
            let at = t.play.world().and_then(|world| world.tick_place(actor)).map(|(location, _)| location.map(|c| c.round()));
            bevy::log::info!(target: "p2_engine::particles", "added {name} at {at:?}: {}", draws.iter().map(|draw| draw.name.as_str()).collect::<Vec<_>>().join("; "));
            t.times.add("spawned: emitters: log", started);
            let started = std::time::Instant::now();
            let seed = t.particles.actors.len() as u32 + 1;
            t.particles.actors.push(ParticleActor { actor, simulation: Emitter::new(emitter.settings, props, seed), parent, draws, objects, bounds: None, buffers: DrawBuffers::default(), zone_box: None, overlay: None, name, zone_seen: true, drawn: true, born: std::time::Instant::now(), peak: 0, frames: ParticleFrames::default(), turn: Turn::default(), puddle_seed: emitter.puddle.then(|| liquid_flow::puddle_seed(seed)) });
            t.times.add("spawned: emitters: simulation", started);
            let started = std::time::Instant::now();
            if let Some(world) = t.play.world_mut() {
                world.watch(actor);
            }
            t.times.add("spawned: emitters: watch", started);
        }
        t.particles.index_particle_emitters();
    }
}

impl Particles {
    fn index_particle_emitters(&mut self) {
        self.particle_emitters.clear();
        for (at, p) in self.actors.iter().enumerate() {
            for (index, &object) in p.objects.iter().enumerate() {
                self.particle_emitters.entry(object).or_insert((at, index));
            }
        }
    }

    pub fn particle_counts(&self) -> (usize, usize) {
        (self.actors.len(), self.actors.iter().flat_map(|p| p.simulation.emitters()).map(|e| e.live_particles()).sum())
    }

    pub fn particles_in_view(&self, world: &PlayWorld, camera: &Transform, range: f32, count: usize) -> Vec<ParticleReport> {
        let eye = Vec3::from_array(camera_view(camera, world.origin()).location.into());
        let forward = Vec3::from(to_bevy((camera.rotation * Vec3::NEG_Z).to_array()));
        let mut near: Vec<(f32, ParticleReport)> = (self.actors.iter())
            .filter_map(|p| {
                let actor = world.actor(p.actor)?;
                let offset = Vec3::from_array(world.place(p.actor)?.0.into()) - eye;
                let distance = offset.length();
                if distance > range {
                    return None;
                }
                let angle = if distance < 1.0 { 0.0 } else { forward.angle_between(offset) };
                let emitters = (p.simulation.emitters().iter().zip(&p.draws))
                    .map(|(e, EmitterDraw { name, .. })| {
                        let limit = e.props().seconds_before_inactive;
                        let paused = if limit > 0.0 && e.inactive_for() > limit { ", paused" } else { "" };
                        format!("{:?} {name}: {} live{paused}", e.props().kind, e.live_particles())
                    })
                    .collect();
                let hidden = actor.hidden;
                Some((angle, ParticleReport { name: p.name.clone(), distance, hidden, unseen: !p.drawn, zone_culled: !p.zone_seen, overlay: p.overlay, emitters }))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.into_iter().take(count).map(|(_, report)| report).collect()
    }

    fn particle_emitter(&mut self, emitter: ObjectId) -> Option<(&mut ParticleActor, usize)> {
        let &(actor, index) = self.particle_emitters.get(&emitter)?;
        Some((self.actors.get_mut(actor)?, index))
    }

    fn apply_vm_changes(&mut self, world: &mut PlayWorld) {
        for (emitter, properties) in world.take_emitter_changes() {
            if let Some((particles, index)) = self.particle_emitter(emitter) {
                particles.simulation.set_properties(index, properties);
            }
        }
        for command in world.take_particle_commands() {
            let target = match command {
                ParticleCommand::Reset(e, _)
                | ParticleCommand::Spawn { emitter: e, .. }
                | ParticleCommand::CapTime { emitter: e, .. }
                | ParticleCommand::KeepAlive(e) => {
                    self.particle_emitter(e)
                }
                ParticleCommand::Kill(actor) => self.actors.iter_mut().find(|p| p.actor == actor).map(|p| (p, 0)),
            };
            if let Some((particles, index)) = target {
                apply_command(&mut particles.simulation, index, command);
            }
        }
    }

    pub(super) fn attach_late_pools(&mut self, model: Option<&SpawnModel>, key: &ModelKey, assets: &mut AssetStores) {
        for p in &mut self.actors {
            for (index, draw) in p.draws.iter_mut().enumerate() {
                if draw.pending_model.as_ref() != Some(key) {
                    continue;
                }
                draw.pending_model = None;
                if let (Some(SpawnModel::Static { sections, .. }), Some(emitter)) = (model, p.simulation.emitters().get(index)) {
                    draw.pool = Some(MeshPool::new(sections, emitter.props(), assets, &mut self.pool_materials));
                }
            }
        }
    }
}

/// One emitter's frame. On a bone and not placed yet it is neither drawn nor stepped: where the game keeps its actor is
/// where it was spawned, the weapon's origin by the eye, and a sprite left there fills the screen.
fn emitter_turn(place: Option<(Vector, p2_math::Rotator)>, actor: &crate::play::ActorFrame, step: f32, shown: f32) -> Turn {
    let (location, rotation) = place.unwrap_or_default();
    // Player-owned emitters ignore `TimeDilation`. One simulation tick per game tick, as the engine's: host frames without a
    // new game frame (step 0) do not tick. A tick with no time still runs the "all particles dead" test, and an emitter dies
    // on its second dead tick (`AutoDestroy`): an empty gas trail, whose first splat comes one game tick later, died before
    // it.
    Turn {
        owner: Owner { location, rotation, velocity: actor.velocity },
        place: place.map(|(location, _)| location),
        step: (step > 0.0 && place.is_some()).then_some(step * actor.time_scale),
        shown,
        hidden: actor.hidden || place.is_none(),
        here: Vec3::from(to_bevy(location)),
        seen: false,
        seen_box: None,
    }
}

pub(super) struct ParticleTargets<'a, 'w, 's> {
    pub(super) particles: &'a mut Particles,
    pub(super) drawn: &'a mut DrawnParticles,
    pub(super) play: &'a mut Play,
    pub(super) canvas: &'a [CanvasActor],
    pub(super) view: &'a mut ParticleView<'w, 's>,
    pub(super) commands: &'a mut Commands<'w, 's>,
}

#[derive(SystemParam)]
pub(super) struct ParticleParams<'w, 's> {
    particles: ResMut<'w, Particles>,
    drawn: ResMut<'w, DrawnParticles>,
    play: ResMut<'w, Play>,
    hud: Res<'w, Hud>,
    view: ParticleView<'w, 's>,
    commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_particles(mut t: ParticleParams) {
    let started = std::time::Instant::now();
    update_particles(&mut ParticleTargets {
        particles: &mut t.particles,
        drawn: &mut t.drawn,
        play: &mut t.play,
        canvas: &t.hud.canvas_actors,
        view: &mut t.view,
        commands: &mut t.commands,
    });
    t.times.add("particles", started);
}

pub(super) fn update_particles(t: &mut ParticleTargets) {
    t.drawn.born.append(&mut t.particles.born);
    let step = t.play.world_mut().map_or(0.0, PlayWorld::take_step);
    if let Some(world) = t.play.world_mut() {
        t.particles.apply_vm_changes(world);
    }
    let Some(world) = t.play.world_mut() else { return };
    let Some(camera) = t.view.cameras.iter().next() else { return };
    let view = camera_view(camera, world.origin());
    let eye = Vec3::from_array(view.location.into());
    let collision = t.view.collision.0.clone();
    let leaves = collision.as_deref().map(|world| &world.model).filter(|model| !model.leaves.is_empty());
    let (mut removed, mut finished) = (Vec::new(), Vec::new());
    let forces: Arc<[_]> = world.actor_forces().into();
    let in_frustum = frustum_test(&t.view.cameras, &t.view.projections, world.origin());
    // Hung on a bone of a HUD-drawn actor (the muzzle flash): retail draws a skeletal actor's attachments
    // in its own scene node, so `Canvas.DrawActor` draws them at its FOV after its depth clear.
    let canvas = t.canvas;
    let particles = &mut *t.particles;
    let shown = 1.0 - world.shown.clock.behind();
    t.drawn.eye = eye;
    particles.actors.retain_mut(|p| {
        let Some(actor) = world.actor(p.actor) else { return true };
        if actor.gone || p.simulation.destroyed() {
            let why = if actor.gone { "destroyed by script" } else { "emitters finished" };
            log_removed(p, why);
            t.commands.entity(p.parent).despawn();
            removed.push(p.actor);
            // Retail destroys the actor itself here: its `Destroyed` runs (a `FluidTrail` clears its feeder's
            // `FTrail`, so the pour starts a new trail).
            if !actor.gone {
                finished.push(p.actor);
            }
            return false;
        }
        let live: usize = p.simulation.emitters().iter().map(|e| e.live_particles()).sum();
        p.peak = p.peak.max(live);
        sync_disabled(p, &actor.emitters_disabled);
        // Off screen longer than `SecondsBeforeInactive`, emitters pause. An emitter without particles counts
        // as drawn, or it would never spawn; what retail does with an empty emitter's bounds is (unverified).
        let overlay = actor.attachment.as_ref().and_then(|attachment| overlay_slot(world, canvas, attachment.base));
        if p.overlay != overlay {
            p.overlay = overlay;
            let layer = overlay.map_or_else(RenderLayers::default, |slot| RenderLayers::layer(OVERLAY_LAYERS[slot]));
            for entity in p.draws.iter().filter_map(|draw| draw.pool.as_ref()).flat_map(|pool| pool.slots.iter().flatten()) {
                t.commands.entity(*entity).insert(layer.clone());
            }
        }
        let turn = emitter_turn(world.place(p.actor), actor, step, shown);
        let (hidden, here) = (turn.hidden, turn.here);
        let zone_seen = t.view.zone_culled.get(p.parent).ok().is_none_or(|shown| *shown != Visibility::Hidden);
        let with_here = |bounds: Option<(Vec3, Vec3)>| bounds.map(|(min, max)| (min.min(here), max.max(here)));
        let in_frustum_now = in_frustum(with_here(p.bounds));
        let drawn = !hidden && zone_seen && in_frustum_now;
        (p.zone_seen, p.drawn) = (zone_seen, drawn);
        if live > 0 {
            let frames = &mut p.frames;
            *(if drawn { &mut frames.drawn } else if hidden { &mut frames.hidden } else if !in_frustum_now { &mut frames.out_of_view } else { &mut frames.zone_unseen }) += 1;
        }
        for emitter in p.simulation.emitters_mut() {
            if drawn || emitter.live_particles() == 0 {
                emitter.mark_rendered();
            }
        }
        if p.simulation.emitters().iter().any(|e| e.props().use_actor_forces) {
            p.simulation.set_actor_forces(forces.clone());
        }
        p.turn = turn;
        true
    });
    let simulate = |(p, slot): (&mut ParticleActor, &mut DrawnActor)| {
        let mut collide = |start: Vector, end: Vector| collision.as_deref().and_then(|world| particle_collision(world, start, end));
        if let Some(step) = p.turn.step {
            p.simulation.tick_colliding(step, p.turn.owner, &mut collide);
        }
        p.turn.seen_box = if p.turn.hidden { None } else { particles_box(p) };
        p.turn.seen = !p.turn.hidden && p.zone_seen && in_frustum(p.turn.seen_box);
        let mut buffers = std::mem::take(&mut p.buffers);
        fill_draw_buffers(p, &mut buffers, slot, p.turn.seen, view);
        p.buffers = buffers;
    };
    let slots = &mut t.drawn.actors;
    slots.resize_with(particles.actors.len(), Default::default);
    let pool = bevy::tasks::ComputeTaskPool::get();
    if particles.actors.len() < PARALLEL_EMITTERS {
        particles.actors.iter_mut().zip(slots.iter_mut()).for_each(simulate);
    } else {
        let share = particles.actors.len().div_ceil(pool.thread_num().max(1)).max(PARALLEL_EMITTERS / 2);
        let simulate = &simulate;
        pool.scope(|scope| {
            for (emitters, slots) in particles.actors.chunks_mut(share).zip(slots.chunks_mut(share)) {
                scope.spawn(async move { emitters.iter_mut().zip(slots).for_each(simulate) });
            }
        });
    }
    for p in &mut particles.actors {
        let Turn { here, seen, seen_box, .. } = p.turn;
        let buffers = std::mem::take(&mut p.buffers);
        place_mesh_particles(t.commands, t.view, p, &buffers.meshes, seen);
        p.buffers = buffers;
        p.bounds = seen_box;
        apply_zone(t.commands, p, (leaves, here));
    }
    if !removed.is_empty() {
        particles.index_particle_emitters();
    }
    world.wants_forces = particles.actors.iter().any(|p| p.simulation.emitters().iter().any(|e| e.props().use_actor_forces));
    for actor in removed {
        world.unwatch(actor);
    }
    world.finished.extend(finished);
}

fn log_removed(p: &ParticleActor, why: &str) {
    bevy::log::info!(
        target: "p2_engine::particles",
        "removed {} ({why}) after {:.2} s, peak {} particles; frames with particles: drawn {}, in view but zone not seen {}, hidden {}, out of view {}",
        p.name,
        p.born.elapsed().as_secs_f32(),
        p.peak,
        p.frames.drawn,
        p.frames.zone_unseen,
        p.frames.hidden,
        p.frames.out_of_view
    );
}

fn sync_disabled(p: &mut ParticleActor, disabled: &[bool]) {
    for (index, &now) in disabled.iter().enumerate() {
        if let Some(draw) = p.draws.get_mut(index)
            && draw.disabled != now
        {
            draw.disabled = now;
            if let Some(emitter) = p.simulation.emitters_mut().get_mut(index) {
                emitter.set_disabled(now);
            }
        }
    }
}

fn overlay_slot(world: &PlayWorld, canvas: &[CanvasActor], base: ObjectId) -> Option<usize> {
    let mut at = base;
    for _ in 0..8 {
        if let Some(shown) = canvas.iter().find(|shown| shown.actor == at) {
            return shown.slot;
        }
        at = world.actor(at)?.attachment.as_ref()?.base;
    }
    None
}

#[cfg(test)]
mod tests;
