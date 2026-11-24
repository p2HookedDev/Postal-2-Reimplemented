use std::time::Instant;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::{NoFrustumCulling, Visibility};
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{ChildOf, Commands, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy::ecs::system::SystemParam;
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::render::storage::ShaderBuffer;
use bevy::transform::components::Transform;
use p2_particles::{EmitterKind, StripPoint};
use rustc_hash::FxHashMap;

use super::particle_frame::{DrawnActor, DrawnEmitter, DrawnParticles, EmitterLook, EmitterShape, MeshEmitterLook};
use super::particle_material::{PARTICLE_SPRITES, ParticleMaterial, RECORD_WORDS, SharedParticleKey, sprite_light};
use super::translucent_order::TranslucentDraw;
use crate::components::OverlaySlot;
use crate::fire_volume::VolumeFires;
use super::bsp_light::FrameLights;
use super::frame_facts::FrameFacts;
use super::glow_lights::GlowLights;
use p2_host::frame_times::{FrameTimes, MeshWrite, mesh_written};
use p2_host::display::{MODERN_LIQUID_FLOW, MODERN_LIQUIDS, Settings};
use bounds::apply_bounds;
use fire::is_fire;
use light::{ActorLight, ParticleLights};
use mesh_particles::{MeshPool, PoolMaterialKey, PoolSlot};
use sprites::{MIN_QUADS, SpriteGrid, grid_capacity, sprite_box, sprite_grid, upload_records, write_sprite_records};
use strips::{empty_quads, fill_beams, fill_smooth_strip, fill_strip};

pub mod bounds;
pub(crate) mod fire;
pub(crate) mod light;
mod mesh_particles;
pub mod sprites;
pub(crate) mod strips;

pub struct ParticleDrawPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParticleDrawSystems;

impl Plugin for ParticleDrawPlugin {
    fn build(&self, app: &mut App) {
        init_drawing_resources(app);
        app.add_systems(PostUpdate, draw_particles.in_set(ParticleDrawSystems));
    }
}

#[derive(Resource, Default)]
pub struct ParticleDraws {
    pub(crate) actors: FxHashMap<Entity, ActorDraw>,
    pub records: Vec<[u32; RECORD_WORDS]>,
    materials: FxHashMap<SharedParticleKey, Handle<ParticleMaterial>>,
    pool_materials: FxHashMap<PoolMaterialKey, Handle<StandardMaterial>>,
    level: u32,
}

#[derive(Default)]
pub(crate) struct ActorDraw {
    emitters: Vec<EmitterDraw>,
    drawn_bounds: Option<(Vec3, Vec3)>,
    overlay: Option<usize>,
    light: ActorLight,
}

#[derive(SystemParam)]
pub struct ParticleAssets<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<ParticleMaterial>>,
    standard_materials: ResMut<'w, Assets<StandardMaterial>>,
    sprite_buffers: Option<ResMut<'w, Assets<ShaderBuffer>>>,
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub fn draw_particles(
    mut drawn: ResMut<DrawnParticles>,
    mut draws: ResMut<ParticleDraws>,
    mut assets: ParticleAssets,
    mut commands: Commands,
    parents: Query<(), With<Transform>>,
    mut pool_slots: Query<&mut Transform, (With<PoolSlot>, Without<crate::lamps::Lamp>)>,
    settings: Option<Res<Settings>>,
    lights: ParticleLights,
    volume_fires: Option<Res<VolumeFires>>,
    mut times: ResMut<FrameTimes>,
) {
    let started = Instant::now();
    let (drawn, draws) = (&mut *drawn, &mut *draws);
    if draws.level != drawn.level {
        draws.level = drawn.level;
        draws.actors.clear();
        draws.materials.clear();
        draws.pool_materials.clear();
    }
    for look in drawn.born.drain(..) {
        draws.add_emitter(look, &mut assets, &mut commands);
    }
    for look in drawn.born_meshes.drain(..) {
        draws.add_mesh_emitter(look, &mut assets);
    }
    draws.actors.retain(|parent, _| parents.contains(*parent));
    let flow = settings.as_deref().is_some_and(|settings| settings.modern_level(MODERN_LIQUID_FLOW) > 0);
    let liquids = settings.as_deref().is_some_and(|settings| settings.modern_level(MODERN_LIQUIDS) > 0);
    let lit_by = lights.wanted(settings.as_deref());
    let mut records = std::mem::take(&mut draws.records);
    records.clear();
    for slot in &drawn.actors {
        let Some(actor) = draws.actors.get_mut(&slot.parent) else { continue };
        lights.follow(&mut actor.light, slot, lit_by);
        if actor.overlay != slot.overlay {
            actor.overlay = slot.overlay;
            let layer = OverlaySlot(slot.overlay);
            let meshes = actor.emitters.iter().filter_map(|draw| draw.mesh.as_ref()).map(|mesh| mesh.entity);
            let slots = actor.emitters.iter().filter_map(|draw| draw.pool.as_ref()).flat_map(MeshPool::slots);
            for entity in meshes.chain(slots) {
                commands.entity(entity).insert(layer);
            }
        }
        for (order, (draw, emitter)) in actor.emitters.iter_mut().zip(&slot.emitters).enumerate() {
            if let Some(pool) = &mut draw.pool {
                pool.place(&mut commands, &mut pool_slots, slot, order as u32, &emitter.meshes);
            }
        }
        let mut writes =
            MeshWrites { commands: &mut commands, meshes: &mut assets.meshes, records: &mut records, liquid_flow: flow, liquids, volume_fires: volume_fires.as_deref() };
        let (bounds, newly_shown) = fill_emitter_meshes(&mut writes, actor, slot, drawn.eye);
        write_sprite_records(&mut writes, actor, slot, bounds);
        let changed = newly_shown || bounds != actor.drawn_bounds;
        actor.drawn_bounds = bounds;
        apply_bounds(&mut writes, actor, changed);
    }
    if !records.is_empty() && let Some(buffers) = assets.sprite_buffers.as_deref_mut() {
        upload_records(buffers, &PARTICLE_SPRITES, &records);
    }
    draws.records = records;
    times.add("particle drawing", started);
}

impl ParticleDraws {
    pub(crate) fn light_of(&self, parent: Entity) -> Option<Vec3> {
        self.actors.get(&parent).map(|actor| actor.light.header()[0])
    }

    fn add_emitter(&mut self, look: EmitterLook, assets: &mut ParticleAssets, commands: &mut Commands) {
        let EmitterLook { parent, order, image, props, name, fogless } = look;
        let grid = (!matches!(props.kind, EmitterKind::Strip | EmitterKind::Beam)).then_some(SpriteGrid { quads: MIN_QUADS, first: None });
        let mesh = if grid.is_some() {
            sprite_grid(MIN_QUADS)
        } else {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            empty_quads(&mut mesh);
            mesh
        };
        let mesh = assets.meshes.add(mesh);
        // Hidden until it has particles; the mesh changes every frame, so it is never frustum culled by stale
        // bounds. Its `Aabb` (the actor's, set with the mesh each frame) is for GPU occlusion culling.
        let model = (Mesh3d(mesh.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling, ChildOf(parent));
        let mut material = ParticleMaterial::new(image, &props);
        material.light = sprite_light(props.draw_style, &name, props.use_direction_as);
        if fogless {
            material.params.fogged = 0;
        }
        let part = TranslucentDraw::Part { actor: parent, order };
        let material = self.materials.entry(material.shared_key()).or_insert_with(|| assets.materials.add(material)).clone();
        let entity = commands.spawn((model, MeshMaterial3d(material), part)).id();
        *self.emitter(parent, order) = EmitterDraw {
            mesh: Some(EmitterMesh { entity, mesh, shown: false, strip_key: None, written: false }),
            grid,
            fire: is_fire(&name, props.draw_style),
            pool: None,
        };
    }

    fn add_mesh_emitter(&mut self, look: MeshEmitterLook, assets: &mut ParticleAssets) {
        let MeshEmitterLook { parent, order, sections, props } = look;
        let pool = MeshPool::new(&sections, &props, &mut assets.standard_materials, &mut self.pool_materials);
        self.emitter(parent, order).pool = Some(pool);
    }

    fn emitter(&mut self, parent: Entity, order: u32) -> &mut EmitterDraw {
        let emitters = &mut self.actors.entry(parent).or_default().emitters;
        let at = order as usize;
        if emitters.len() <= at {
            emitters.resize_with(at + 1, Default::default);
        }
        &mut emitters[at]
    }
}

#[derive(Default)]
struct EmitterDraw {
    mesh: Option<EmitterMesh>,
    grid: Option<SpriteGrid>,
    fire: bool,
    pool: Option<MeshPool>,
}

struct EmitterMesh {
    entity: Entity,
    mesh: Handle<Mesh>,
    shown: bool,
    /// What the strip in the mesh was made from: a strip that lies still (a blood trail) is not written again, as
    /// every write is uploaded.
    strip_key: Option<u64>,
    written: bool,
}

fn strip_key(points: &[StripPoint], facing: Vec3) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut key = rustc_hash::FxHasher::default();
    for point in points {
        (point.location.x.to_bits(), point.location.y.to_bits(), point.location.z.to_bits(), point.size.to_bits(), point.color).hash(&mut key);
    }
    facing.to_array().map(f32::to_bits).hash(&mut key);
    key.finish()
}

struct MeshWrites<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    meshes: &'a mut Assets<Mesh>,
    records: &'a mut Vec<[u32; RECORD_WORDS]>,
    liquid_flow: bool,
    liquids: bool,
    volume_fires: Option<&'a VolumeFires>,
}

fn union(bounds: Option<(Vec3, Vec3)>, (min, max): (Vec3, Vec3)) -> (Vec3, Vec3) {
    bounds.map_or((min, max), |(lo, hi)| (lo.min(min), hi.max(max)))
}

fn fill_emitter_meshes(writes: &mut MeshWrites, actor: &mut ActorDraw, slot: &DrawnActor, eye: Vec3) -> (Option<(Vec3, Vec3)>, bool) {
    let (mut bounds, mut newly_shown) = (slot.mesh_reach, false);
    let flow = writes.liquid_flow;
    // Our own liquid stands in for every game texture of a liquid actor.
    let ours = writes.liquids && slot.liquid.is_some();
    for (order, (draw, DrawnEmitter { sprites, strip, beams, shape, .. })) in actor.emitters.iter_mut().zip(&slot.emitters).enumerate() {
        let EmitterDraw { mesh: Some(EmitterMesh { entity, mesh, shown, strip_key: drawn_key, written }), grid, .. } = draw else { continue };
        let EmitterShape { kind, facing, beam_scale, sheets } = *shape;
        let volume = writes.volume_fires.is_some_and(|fires| fires.0.contains(&(slot.parent, order as u32)));
        let visible = slot.seen && !ours && !volume && (strip.len() > 1 || !sprites.is_empty() || !beams.is_empty());
        if *shown != visible {
            *shown = visible;
            newly_shown |= visible;
            writes.commands.entity(*entity).insert(if *shown { Visibility::Inherited } else { Visibility::Hidden });
        }
        if !*shown {
            continue;
        }
        if let Some(grid) = grid.as_mut() {
            let quads = grid_capacity(grid.quads, sprites.len());
            if quads != grid.quads {
                grid.quads = quads;
                if let Some(mut mesh) = writes.meshes.get_mut(&*mesh) {
                    mesh_written(MeshWrite::SpriteGrid);
                    *mesh = sprite_grid(quads);
                }
            }
            for sprite_bounds in sprites.iter().map(sprite_box) {
                bounds = Some(union(bounds, sprite_bounds));
            }
        } else {
            // The switch is in the key so a still trail is redrawn when it moves.
            let key = (kind == EmitterKind::Strip).then(|| strip_key(strip, facing.unwrap_or(eye)) ^ u64::from(flow));
            if (key.is_none() || key != *drawn_key)
                && let Some(mut mesh) = writes.meshes.get_mut(&*mesh)
            {
                let written_as = match (kind, flow) {
                    (EmitterKind::Strip, true) => {
                        fill_smooth_strip(&mut mesh, strip, facing, eye);
                        MeshWrite::Strip
                    }
                    (EmitterKind::Strip, false) => {
                        fill_strip(&mut mesh, strip, facing, eye);
                        MeshWrite::Strip
                    }
                    _ => {
                        fill_beams(&mut mesh, beams, beam_scale, sheets, eye);
                        MeshWrite::Beam
                    }
                };
                mesh_written(written_as);
                *drawn_key = key;
                *written = true;
            }
            if let Some(aabb) = writes.meshes.get(&*mesh).and_then(|mesh| mesh.get_aabb()) {
                bounds = Some(union(bounds, (Vec3::from(aabb.min()), Vec3::from(aabb.max()))));
            }
        }
    }
    (bounds, newly_shown)
}

pub fn init_drawing_resources(app: &mut App) {
    app.init_resource::<DrawnParticles>().init_resource::<ParticleDraws>().init_resource::<FrameTimes>();
    app.init_resource::<FrameFacts>().init_resource::<FrameLights>().init_resource::<GlowLights>();
    app.init_resource::<Assets<Mesh>>().init_resource::<Assets<ParticleMaterial>>().init_resource::<Assets<StandardMaterial>>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::app::Update;

    #[test]
    fn the_born_are_made_once_and_their_draw_state_goes_with_their_parent() {
        let mut app = App::new();
        init_drawing_resources(&mut app);
        app.add_systems(Update, draw_particles);
        let parent = app.world_mut().spawn(Transform::default()).id();
        let look = EmitterLook { parent, order: 0, image: Handle::default(), props: Default::default(), name: String::new(), fogless: false };
        let mut drawn = app.world_mut().resource_mut::<DrawnParticles>();
        drawn.born.push(look);
        drawn.actors.push(DrawnActor { parent, ..DrawnActor::default() });
        app.update();
        app.update();
        assert!(app.world().resource::<DrawnParticles>().born.is_empty());
        let draws = app.world().resource::<ParticleDraws>();
        let Some(mesh) = draws.actors.get(&parent).and_then(|actor| actor.emitters.first()?.mesh.as_ref()) else { panic!("no emitter mesh") };
        assert_eq!(app.world().get::<ChildOf>(mesh.entity).map(ChildOf::parent), Some(parent));
        let mut parts = app.world_mut().query::<&TranslucentDraw>();
        assert_eq!(parts.iter(app.world()).count(), 1, "made once");
        app.world_mut().entity_mut(parent).despawn();
        app.world_mut().resource_mut::<DrawnParticles>().actors.clear();
        app.update();
        assert!(app.world().resource::<ParticleDraws>().actors.is_empty());
    }

    #[test]
    fn mesh_particles_show_a_pool_slot_each_and_the_spare_slots_hide() {
        let mut app = App::new();
        init_drawing_resources(&mut app);
        app.add_systems(Update, draw_particles);
        let parent = app.world_mut().spawn(Transform::default()).id();
        let sections = vec![crate::scene::SectionView::textured(Handle::default(), None)];
        app.world_mut().resource_mut::<DrawnParticles>().born_meshes.push(MeshEmitterLook { parent, order: 0, sections, props: Default::default() });
        let particle = p2_particles::MeshParticle { location: [0.0; 3].into(), spin: [0; 3].into(), frame: [0; 3].into(), size: [1.0; 3].into(), color: [255; 4] };
        let mut shown_and_hidden = |count: usize| {
            let meshes = vec![particle; count];
            app.world_mut().resource_mut::<DrawnParticles>().actors = vec![DrawnActor { parent, emitters: vec![DrawnEmitter { meshes, ..Default::default() }], ..Default::default() }];
            app.update();
            let world = app.world_mut();
            let shown = world.query_filtered::<&Visibility, With<PoolSlot>>().iter(world).filter(|&&slot| slot == Visibility::Inherited).count();
            (shown, world.query_filtered::<(), With<PoolSlot>>().iter(world).count() - shown)
        };
        assert_eq!(shown_and_hidden(3), (3, 0));
        assert_eq!(shown_and_hidden(1), (1, 2));
    }
}
