use bevy::camera::primitives::{Aabb, MeshAabb};
use bevy::math::Vec3A;
use bevy::mesh::MeshTag;
use bevy::render::storage::ShaderBuffer;
use bevy::shape::Aabb3d;
use p2_math::Vector;
use p2_collision::{Hit, line_check, mesh_line_check, terrain_line_check};
use p2_particles::{Beam, CollisionHit, DirectionUsage, EmitterKind, MeshParticle, Owner, StripPoint};

use super::*;
use super::fluids::{camera_view, empty_quads, fill_beams, fill_strip};
use super::spawned::{SpawnModel, static_model, static_model_key};
use crate::mesh_view::alpha_key;
use crate::emitter_changes::apply_command;
use crate::render::particle_material::{PARTICLE_SPRITES, ParticleMaterial, RECORD_WORDS};

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
    drawn_bounds: Option<(Vec3, Vec3)>,
    buffers: DrawBuffers,
    zone_box: Option<[Vec3; 3]>,
    overlay: Option<usize>,
    placed: bool,
    name: String,
    zone_seen: bool,
    drawn: bool,
    born: std::time::Instant,
    peak: usize,
    frames: ParticleFrames,
}

#[derive(Default)]
struct DrawBuffers {
    particles: Vec<(Vec<StripPoint>, Vec<SpriteParticle>)>,
    beams: Vec<Vec<Beam>>,
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
    mesh: Option<EmitterMesh>,
    grid: Option<SpriteGrid>,
    disabled: bool,
    pool: Option<MeshPool>,
    name: String,
}

struct EmitterMesh {
    entity: Entity,
    mesh: Handle<Mesh>,
    shown: bool,
}

struct MeshPool {
    sections: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    slots: Vec<Vec<Entity>>,
    shown: usize,
    reach: f32,
}

impl MeshPool {
    fn new(sections: &[SectionView], props: &p2_particles::EmitterProperties, t: &mut SceneTargets, shared: &mut HashMap<PoolMaterialKey, Handle<StandardMaterial>>) -> Self {
        let reach = sections
            .iter()
            .filter_map(|section| t.assets.meshes.get(&section.mesh)?.get_aabb())
            .map(|aabb| Vec3::from(aabb.min()).abs().max(Vec3::from(aabb.max()).abs()).length())
            .fold(0.0, f32::max);
        let sections = sections
            .iter()
            .map(|section| {
                let two_sided = section.two_sided || props.render_two_sided;
                let alpha_mode = if props.use_mesh_blend_mode { section.alpha_mode } else { standard_alpha(props.draw_style) };
                let key = PoolMaterialKey { texture: section.texture.as_ref().map(Handle::id), alpha: alpha_key(alpha_mode), two_sided };
                let material = shared.entry(key).or_insert_with(|| {
                    t.assets.materials.add(StandardMaterial {
                        base_color_texture: section.texture.clone(),
                        alpha_mode,
                        unlit: true,
                        double_sided: two_sided,
                        cull_mode: if two_sided { None } else { Some(Face::Back) },
                        ..Default::default()
                    })
                });
                (section.mesh.clone(), material.clone())
            })
            .collect();
        Self { sections, slots: Vec::new(), shown: 0, reach }
    }
}

fn mesh_transform(particle: &MeshParticle) -> Transform {
    let turn = rotator_matrix(particle.frame) * rotator_matrix(particle.spin);
    unreal_to_bevy(Mat4::from_translation(Vec3::from_array(particle.location.into())) * turn * Mat4::from_scale(Vec3::from_array(particle.size.into())))
}

fn mesh_box(particle: &MeshParticle, reach: f32) -> (Vec3, Vec3) {
    let at = Vec3::from(to_bevy(particle.location));
    let reach = Vec3::splat(reach * <[f32; 3]>::from(particle.size).iter().fold(0.0_f32, |most, size| most.max(size.abs())));
    (at - reach, at + reach)
}

#[derive(Clone, Copy, Debug)]
struct SpriteGrid {
    quads: u32,
    first: Option<u32>,
}

const MIN_QUADS: u32 = 16;

/// Retail draws a translucent actor's emitters in array order, actors back to front by box centre.
/// Bevy sorts per mesh, so all meshes get the actor's bounds and later emitters this extra bias.
pub fn emitter_sort_bias(index: usize) -> f32 {
    index as f32 * 0.01
}

fn standard_alpha(style: DrawStyle) -> AlphaMode {
    match style {
        DrawStyle::AlphaBlend => AlphaMode::Blend,
        DrawStyle::Modulated | DrawStyle::Darken => AlphaMode::Multiply,
        DrawStyle::Regular => AlphaMode::Mask(0.5),
        DrawStyle::Translucent | DrawStyle::AlphaModulate | DrawStyle::Brighten => AlphaMode::Add,
    }
}

/// Two vertices no triangle uses: the mesh's bounds, and the centre Bevy sorts it by, become the actor's bounds
/// ([`emitter_sort_bias`]). Bevy coordinates.
pub fn pad_bounds(mesh: &mut Mesh, min: Vec3, max: Vec3) {
    use bevy::mesh::VertexAttributeValues::{Float32x2, Float32x3, Float32x4};
    if let Some(Float32x3(positions)) = mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION) {
        positions.extend([min.to_array(), max.to_array()]);
    }
    if let Some(Float32x3(normals)) = mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL) {
        normals.extend([[0.0, 1.0, 0.0]; 2]);
    }
    for uvs in [Mesh::ATTRIBUTE_UV_0, Mesh::ATTRIBUTE_UV_1] {
        if let Some(Float32x2(uvs)) = mesh.attribute_mut(uvs) {
            uvs.extend([[0.0; 2]; 2]);
        }
    }
    if let Some(Float32x4(colors)) = mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR) {
        colors.extend([[0.0; 4]; 2]);
    }
}

/// How far the occlusion-culling `Aabb` reaches past the particle bounds (Unreal units). Flat sprites on a surface
/// (puddles, splats) lie in it or a hair under it, and the culling shader compares the box's nearest depth with the
/// surface's own: from steep angles the floor is in front of the box and the puddle vanishes.
const OCCLUSION_MARGIN: f32 = 8.0;

pub fn occlusion_aabb(min: Vec3, max: Vec3) -> Aabb {
    Aabb::from_min_max(min - Vec3::splat(OCCLUSION_MARGIN), max + Vec3::splat(OCCLUSION_MARGIN))
}

#[cfg(test)]
pub(super) fn fill_sprites(mesh: &mut Mesh, sprites: &[SpriteParticle]) {
    let mut positions = Vec::with_capacity(sprites.len() * 4);
    let mut normals = Vec::with_capacity(sprites.len() * 4);
    let mut uvs = Vec::with_capacity(sprites.len() * 4);
    let mut next_uvs = Vec::with_capacity(sprites.len() * 4);
    let mut colors = Vec::with_capacity(sprites.len() * 4);
    let mut indices = Vec::with_capacity(sprites.len() * 6);
    for sprite in sprites {
        let center = Vec3::from(to_bevy(sprite.location));
        let (up, right) = (Vec3::from(to_bevy(sprite.up)), Vec3::from(to_bevy(sprite.right)));
        let [red, green, blue, alpha] = sprite.color.map(|c| f32::from(c) / 255.0);
        let ([u0, v0, u1, v1], [n0, m0, n1, m1], alpha) = match sprite.blend {
            Some((next, factor)) => (sprite.uv, next, f32::from(factor) / 255.0),
            None => (sprite.uv, sprite.uv, alpha),
        };
        let base = positions.len() as u32;
        positions.extend([center - up - right, center - up + right, center + up + right, center + up - right].map(|corner| corner.to_array()));
        normals.extend([up.cross(right).normalize_or_zero().to_array(); 4]);
        uvs.extend([[u0, v0], [u1, v0], [u1, v1], [u0, v1]]);
        next_uvs.extend([[n0, m0], [n1, m0], [n1, m1], [n0, m1]]);
        colors.extend([[red, green, blue, alpha]; 4]);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    if positions.is_empty() {
        positions.extend([[0.0; 3]; 4]);
        normals.extend([[0.0, 1.0, 0.0]; 4]);
        uvs.extend([[0.0; 2]; 4]);
        next_uvs.extend([[0.0; 2]; 4]);
        colors.extend([[0.0; 4]; 4]);
        indices.extend([0, 1, 2, 0, 2, 3]);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, next_uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

const DRAWN_REACH: f32 = 2.0;

fn particles_box(p: &ParticleActor) -> Option<(Vec3, Vec3)> {
    (p.draws.iter().enumerate())
        .filter_map(|(index, draw)| {
            let reach = match (&draw.pool, p.simulation.emitters().get(index)?.props().kind) {
                (Some(pool), _) => pool.reach,
                (None, EmitterKind::Mesh) => return None,
                (None, _) => DRAWN_REACH,
            };
            let (min, max) = p.simulation.bounds(index, reach)?;
            Some((bevy_vector(min), bevy_vector(max)))
        })
        .reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max)))
}

fn fill_draw_buffers(p: &ParticleActor, buffers: &mut DrawBuffers, seen: bool, view: p2_particles::View) {
    let DrawBuffers { particles, beams, meshes, strip_order } = buffers;
    particles.resize_with(p.draws.len(), Default::default);
    beams.resize_with(p.draws.len(), Vec::new);
    meshes.resize_with(p.draws.len(), Default::default);
    for (index, draw) in p.draws.iter().enumerate() {
        let (strip, sprites) = &mut particles[index];
        let (mesh_particles, reach) = &mut meshes[index];
        strip.clear();
        sprites.clear();
        mesh_particles.clear();
        *reach = 0.0;
        if !seen {
            beams[index].clear();
            continue;
        }
        match p.simulation.emitters().get(index).map(|e| e.props().kind) {
            Some(EmitterKind::Strip) => p.simulation.strip_into(index, strip_order, strip),
            Some(EmitterKind::Mesh | EmitterKind::Beam) | None => {}
            Some(_) => sprites.extend(p.simulation.sprites(index, view)),
        }
        p.simulation.beams_into(index, &mut beams[index]);
        if let Some(pool) = &draw.pool {
            mesh_particles.extend(p.simulation.meshes(index));
            *reach = pool.reach;
        }
    }
}

fn sprite_box(sprite: &SpriteParticle) -> (Vec3, Vec3) {
    let (at, reach) = (bevy_vector(sprite.location), bevy_vector(sprite.up).abs() + bevy_vector(sprite.right).abs());
    (at - reach, at + reach)
}

fn sprite_grid(quads: u32) -> Mesh {
    let positions: Vec<[f32; 3]> = (0..quads).flat_map(|quad| (0..4).map(move |corner| [quad as f32, corner as f32, 0.0])).collect();
    let indices = (0..quads).flat_map(|quad| [0, 1, 2, 0, 2, 3].map(|i| quad * 4 + i)).collect();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    // Transparent meshes sort by this centre, so the entity's transform sets the draw order.
    mesh.final_aabb = Some(Aabb3d { min: Vec3A::ZERO, max: Vec3A::ZERO });
    mesh
}

fn grid_place(min: Vec3, max: Vec3) -> (Transform, Aabb) {
    let centre = (min + max) / 2.0;
    (Transform::from_translation(centre), occlusion_aabb(min - centre, max - centre))
}

fn grid_capacity(current: u32, count: usize) -> u32 {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    if count <= current { current } else { count.next_power_of_two().max(MIN_QUADS) }
}

/// Fills `SpriteRecord` in `particle.wesl`, with the sprite's position relative to `grid_at`.
fn sprite_record(sprite: &SpriteParticle, grid_at: Vec3) -> [u32; RECORD_WORDS] {
    let [r, g, b, a] = sprite.color;
    let (next, alpha) = sprite.blend.unwrap_or((sprite.uv, a));
    let mut record = [0; RECORD_WORDS];
    let floats = |v: Vector| to_bevy(v).map(f32::to_bits);
    record[0..3].copy_from_slice(&(bevy_vector(sprite.location) - grid_at).to_array().map(f32::to_bits));
    record[3] = u32::from_le_bytes([r, g, b, alpha]);
    record[4..7].copy_from_slice(&floats(sprite.up));
    record[8..11].copy_from_slice(&floats(sprite.right));
    record[12..16].copy_from_slice(&sprite.uv.map(f32::to_bits));
    record[16..20].copy_from_slice(&next.map(f32::to_bits));
    record
}

fn header_record(count: usize) -> [u32; RECORD_WORDS] {
    let mut record = [0; RECORD_WORDS];
    record[7] = u32::try_from(count).unwrap_or(u32::MAX);
    record
}

pub(super) fn particle_collision(world: &StaticCollision, start: Vector, end: Vector) -> Option<CollisionHit> {
    let bsp = line_check(&world.model, start, end);
    let terrain = world.terrains.iter().filter_map(|t| terrain_line_check(t, start, end));
    let mesh = mesh_line_check(&world.meshes, start, end);
    let hit: Hit = bsp.into_iter().chain(terrain).chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))?;
    Some(CollisionHit { location: hit.location, normal: hit.normal })
}

impl Session {
    pub(super) fn add_particles(&mut self, emitters: Vec<(ObjectId, EmitterActor)>, origin: Vec3, t: &mut SceneTargets) {
        if emitters.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for (actor, emitter) in emitters {
            let (objects, emitter_meshes, name) = (emitter.objects, emitter.meshes, emitter.name);
            let fogless = emitter.settings.disable_fogging;
            let parent = t.commands.spawn((Transform::from_translation(origin), Visibility::default())).id();
            let mut draws = Vec::new();
            let mut props = Vec::new();
            for (index, ((p, texture), mesh)) in emitter.emitters.into_iter().zip(emitter_meshes).enumerate() {
                let emitter_name = if p.kind == EmitterKind::Mesh { mesh.clone() } else { texture.clone() }.unwrap_or_else(|| "-".into());
                let mut pool = None;
                if p.kind == EmitterKind::Mesh && let Some(path) = mesh {
                    let model = self.spawn_models.entry(static_model_key(&path, &[])).or_insert_with(|| static_model(game, t, &path, &[]));
                    if let Some(SpawnModel::Static { sections, .. }) = model {
                        pool = Some(MeshPool::new(sections, &p, t, &mut self.pool_materials));
                    }
                }
                let image = texture.filter(|_| p.kind != EmitterKind::Mesh).and_then(|path| {
                    let found = self.actor_textures.entry(path).or_insert_with_key(|path| ActorTexture::read(game, path, |image| t.assets.images.add(image)));
                    found.as_ref().map(|texture| texture.image.clone())
                });
                let grid = (image.is_some() && !matches!(p.kind, EmitterKind::Strip | EmitterKind::Beam)).then_some(SpriteGrid { quads: MIN_QUADS, first: None });
                let emitter_mesh = image.map(|image| {
                    let mesh = if grid.is_some() {
                        sprite_grid(MIN_QUADS)
                    } else {
                        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
                        empty_quads(&mut mesh);
                        mesh
                    };
                    let mesh = t.assets.meshes.add(mesh);
                    // Hidden until it has particles; the mesh changes every frame, so it is never frustum culled by stale
                    // bounds. Its `Aabb` (the actor's, set with the mesh each frame) is for GPU occlusion culling.
                    let model = (Mesh3d(mesh.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling, ChildOf(parent));
                    let mut material = ParticleMaterial { sort_bias: emitter_sort_bias(index), ..ParticleMaterial::new(image, &p) };
                    if fogless {
                        material.params.fogged = 0;
                    }
                    let entity = t.commands.spawn((model, MeshMaterial3d(t.assets.particle_materials.add(material)))).id();
                    EmitterMesh { entity, mesh, shown: false }
                });
                draws.push(EmitterDraw { mesh: emitter_mesh, grid, disabled: p.disabled, pool, name: emitter_name });
                props.push(p);
            }
            let at = t.play.world().and_then(|world| world.actor(actor)).map(|frame| frame.location.map(|c| c.round()));
            bevy::log::info!(target: "p2_engine::particles", "added {name} at {at:?}: {}", draws.iter().map(|draw| draw.name.as_str()).collect::<Vec<_>>().join("; "));
            let seed = self.particles.len() as u32 + 1;
            self.particles.push(ParticleActor { actor, simulation: Emitter::new(emitter.settings, props, seed), parent, draws, objects, bounds: None, drawn_bounds: None, buffers: DrawBuffers::default(), zone_box: None, overlay: None, placed: false, name, zone_seen: true, drawn: true, born: std::time::Instant::now(), peak: 0, frames: ParticleFrames::default() });
            if let Some(world) = t.play.world_mut() {
                world.watch(actor);
            }
        }
        self.index_particle_emitters();
    }

    fn index_particle_emitters(&mut self) {
        self.particle_emitters.clear();
        for (at, p) in self.particles.iter().enumerate() {
            for (index, &object) in p.objects.iter().enumerate() {
                self.particle_emitters.entry(object).or_insert((at, index));
            }
        }
    }

    pub fn particle_counts(&self) -> (usize, usize) {
        (self.particles.len(), self.particles.iter().flat_map(|p| p.simulation.emitters()).map(|e| e.live_particles()).sum())
    }

    pub fn particles_in_view(&self, world: &PlayWorld, camera: &Transform, range: f32, count: usize) -> Vec<ParticleReport> {
        let eye = Vec3::from_array(camera_view(camera, world.origin()).location.into());
        let forward = Vec3::from(to_bevy((camera.rotation * Vec3::NEG_Z).to_array()));
        let mut near: Vec<(f32, ParticleReport)> = (self.particles.iter())
            .filter_map(|p| {
                let actor = world.actor(p.actor)?;
                let offset = Vec3::from_array(actor.location.into()) - eye;
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
                let hidden = actor.hidden || !p.placed;
                Some((angle, ParticleReport { name: p.name.clone(), distance, hidden, unseen: !p.drawn, zone_culled: !p.zone_seen, overlay: p.overlay, emitters }))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.into_iter().take(count).map(|(_, report)| report).collect()
    }

    fn particle_emitter(&mut self, emitter: ObjectId) -> Option<(&mut ParticleActor, usize)> {
        let &(actor, index) = self.particle_emitters.get(&emitter)?;
        Some((self.particles.get_mut(actor)?, index))
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
                ParticleCommand::Kill(actor) => self.particles.iter_mut().find(|p| p.actor == actor).map(|p| (p, 0)),
            };
            if let Some((particles, index)) = target {
                apply_command(&mut particles.simulation, index, command);
            }
        }
    }

    pub(super) fn update_particles(&mut self, t: &mut SceneTargets) {
        let step = t.play.world_mut().map_or(0.0, PlayWorld::take_step);
        if let Some(world) = t.play.world_mut() {
            self.apply_vm_changes(world);
        }
        let Some(world) = t.play.world_mut() else { return };
        let Some(camera) = t.particle_view.cameras.iter().next() else { return };
        let view = camera_view(camera, world.origin());
        let eye = Vec3::from_array(view.location.into());
        let collision = t.particle_view.collision.0.clone();
        let mut collide = |start: Vector, end: Vector| collision.as_deref().and_then(|world| particle_collision(world, start, end));
        let leaves = collision.as_deref().map(|world| &world.model).filter(|model| !model.leaves.is_empty());
        let (mut removed, mut finished) = (Vec::new(), Vec::new());
        let forces: Arc<[_]> = world.actor_forces().into();
        let in_frustum = t.particle_view.frustum_test(world.origin());
        // Hung on a bone of a HUD-drawn actor (the muzzle flash): retail draws a skeletal actor's attachments
        // in its own scene node, so `Canvas.DrawActor` draws them at its FOV after its depth clear.
        let canvas = &self.canvas_actors;
        let mut records = std::mem::take(&mut self.particle_records);
        records.clear();
        self.particles.retain_mut(|p| {
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
                for mesh in p.draws.iter().filter_map(|draw| draw.mesh.as_ref()) {
                    t.commands.entity(mesh.entity).insert(layer.clone());
                }
                for entity in p.draws.iter().filter_map(|draw| draw.pool.as_ref()).flat_map(|pool| pool.slots.iter().flatten()) {
                    t.commands.entity(*entity).insert(layer.clone());
                }
            }
            // Retail places an attachment as it draws it, the VM learns the place a frame later. Before its first place it is not
            // drawn: spawned at the weapon's origin by the eye, its sprite would fill the screen.
            let place = t.particle_view.places.as_ref().and_then(|places| places.0.get(&p.actor)).map(|&(_, place)| place.location);
            p.placed |= place.is_some() || actor.attachment.is_none();
            let hidden = actor.hidden || !p.placed;
            let zone_seen = t.particle_view.zone_culled.get(p.parent).ok().is_none_or(|shown| *shown != Visibility::Hidden);
            let here = Vec3::from(to_bevy(actor.location));
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
            let owner = Owner { location: place.unwrap_or(actor.location), rotation: actor.rotation, velocity: actor.velocity };
            // Player-owned emitters ignore `TimeDilation`. One simulation tick per game tick, as the engine's: host frames without a
            // new game frame (step 0) do not tick. A tick with no time still runs the "all particles dead" test, and an emitter dies
            // on its second dead tick (`AutoDestroy`): an empty gas trail, whose first splat comes one game tick later, died before
            // it.
            if step > 0.0 {
                p.simulation.tick_colliding(step * actor.time_scale, owner, &mut collide);
            }
            let seen_box = if hidden { None } else { particles_box(p) };
            let seen = !hidden && zone_seen && in_frustum(seen_box);
            let mut buffers = std::mem::take(&mut p.buffers);
            fill_draw_buffers(p, &mut buffers, seen, view);
            let mesh_reach = (buffers.meshes.iter())
                .flat_map(|(particles, reach)| particles.iter().map(move |m| mesh_box(m, *reach)))
                .reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max)));
            let mut writes = MeshWrites { commands: &mut t.commands, meshes: &mut t.assets.meshes, records: &mut records };
            let (drawn, newly_shown) = fill_emitter_meshes(&mut writes, p, &buffers.particles, &buffers.beams, seen, eye, mesh_reach);
            write_sprite_records(&mut writes, p, &buffers.particles, drawn);
            place_mesh_particles(&mut t.commands, p, &buffers.meshes, seen);
            p.buffers = buffers;
            let changed = newly_shown || drawn != p.drawn_bounds;
            p.drawn_bounds = drawn;
            p.bounds = seen_box;
            let mut writes = MeshWrites { commands: &mut t.commands, meshes: &mut t.assets.meshes, records: &mut records };
            apply_bounds(&mut writes, p, changed, (leaves, here));
            true
        });
        if !removed.is_empty() {
            self.index_particle_emitters();
        }
        if !records.is_empty() && let Some(buffers) = t.assets.sprite_buffers.as_mut() {
            upload_sprites(buffers, &records);
        }
        self.particle_records = records;
        world.wants_forces = self.particles.iter().any(|p| p.simulation.emitters().iter().any(|e| e.props().use_actor_forces));
        for actor in removed {
            world.unwatch(actor);
        }
        world.finished.extend(finished);
    }
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

fn place_mesh_particles(commands: &mut Commands, p: &mut ParticleActor, meshes: &[(Vec<MeshParticle>, f32)], in_view: bool) {
    let (overlay, parent) = (p.overlay, p.parent);
    for (draw, (particles, _)) in p.draws.iter_mut().zip(meshes) {
        let Some(pool) = &mut draw.pool else { continue };
        let shown = if in_view { particles.len() } else { 0 };
        while pool.slots.len() < shown {
            let layer = overlay.map_or_else(RenderLayers::default, |slot| RenderLayers::layer(OVERLAY_LAYERS[slot]));
            let slot: Vec<Entity> = (pool.sections.iter())
                .map(|(mesh, material)| {
                    let section = (Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), Transform::default(), Visibility::Hidden);
                    commands.spawn((section, layer.clone(), ChildOf(parent))).id()
                })
                .collect();
            pool.slots.push(slot);
        }
        for (index, slot) in pool.slots.iter().enumerate() {
            let now = index < shown;
            let visibility = (now != (index < pool.shown)).then_some(if now { Visibility::Inherited } else { Visibility::Hidden });
            let transform = particles.get(index).filter(|_| now).map(mesh_transform);
            for &entity in slot {
                let mut entity = commands.entity(entity);
                if let Some(transform) = transform {
                    entity.insert(transform);
                }
                if let Some(visibility) = visibility {
                    entity.insert(visibility);
                }
            }
        }
        pool.shown = shown;
    }
}

struct MeshWrites<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    meshes: &'a mut Assets<Mesh>,
    records: &'a mut Vec<[u32; RECORD_WORDS]>,
}

fn fill_emitter_meshes(
    writes: &mut MeshWrites,
    p: &mut ParticleActor,
    particles: &[(Vec<StripPoint>, Vec<SpriteParticle>)],
    beams: &[Vec<Beam>],
    in_view: bool,
    eye: Vec3,
    mut bounds: Option<(Vec3, Vec3)>,
) -> (Option<(Vec3, Vec3)>, bool) {
    let mut newly_shown = false;
    for (index, ((draw, (strip, sprites)), beams)) in p.draws.iter_mut().zip(particles).zip(beams).enumerate() {
        let EmitterDraw { mesh: Some(EmitterMesh { entity, mesh, shown }), grid, .. } = draw else { continue };
        let Some(props) = p.simulation.emitters().get(index).map(|e| e.props()) else { continue };
        let (kind, usage, normal) = (props.kind, props.use_direction_as, Vec3::from_array(props.real_projection_normal.into()));
        let visible = in_view && (strip.len() > 1 || !sprites.is_empty() || !beams.is_empty());
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
                    *mesh = sprite_grid(quads);
                }
            }
            for (min, max) in sprites.iter().map(sprite_box) {
                bounds = Some(bounds.map_or((min, max), |(lo, hi)| (lo.min(min), hi.max(max))));
            }
        } else if let Some(mut mesh) = writes.meshes.get_mut(&*mesh) {
            if kind == EmitterKind::Strip {
                fill_strip(&mut mesh, strip, (usage != DirectionUsage::None).then(|| normal.normalize_or_zero()), eye);
            } else {
                fill_beams(&mut mesh, beams, [props.beam.u_scale, props.beam.v_scale], props.beam.sheets(), eye);
            }
            if let Some(aabb) = mesh.get_aabb() {
                let (min, max) = (Vec3::from(aabb.min()), Vec3::from(aabb.max()));
                bounds = Some(bounds.map_or((min, max), |(lo, hi)| (lo.min(min), hi.max(max))));
            }
        }
    }
    (bounds, newly_shown)
}

fn write_sprite_records(writes: &mut MeshWrites, p: &mut ParticleActor, particles: &[(Vec<StripPoint>, Vec<SpriteParticle>)], drawn: Option<(Vec3, Vec3)>) {
    let grid_at = drawn.map_or(Vec3::ZERO, |(min, max)| (min + max) / 2.0);
    for (draw, (_, sprites)) in p.draws.iter_mut().zip(particles) {
        let (Some(EmitterMesh { entity, shown: true, .. }), Some(grid)) = (&draw.mesh, &mut draw.grid) else { continue };
        let first = u32::try_from(writes.records.len()).unwrap_or(u32::MAX);
        writes.records.push(header_record(sprites.len()));
        writes.records.extend(sprites.iter().map(|sprite| sprite_record(sprite, grid_at)));
        if grid.first != Some(first) {
            grid.first = Some(first);
            writes.commands.entity(*entity).insert(MeshTag::new(first));
        }
    }
}

fn apply_bounds(writes: &mut MeshWrites, p: &mut ParticleActor, changed: bool, (leaves, here): (Option<&Model>, Vec3)) {
    if let Some((min, max)) = p.drawn_bounds {
        for draw in &p.draws {
            let Some(EmitterMesh { entity, mesh, shown: true }) = &draw.mesh else { continue };
            if draw.grid.is_some() {
                if changed {
                    writes.commands.entity(*entity).insert(grid_place(min, max));
                }
                continue;
            }
            if let Some(mut mesh) = writes.meshes.get_mut(mesh) {
                pad_bounds(&mut mesh, min, max);
            }
            writes.commands.entity(*entity).insert(occlusion_aabb(min, max));
        }
    }
    // The parent is at the Unreal origin: the box swapped back is in Unreal space. Grown like the occlusion box: a flat
    // puddle a hair under its floor lay in solid space only, touched no zone and stayed hidden.
    if let (Some(model), Some((min, max))) = (leaves, p.bounds)
        && p.zone_box != Some([min, max, here])
    {
        p.zone_box = Some([min, max, here]);
        let (min, max) = (min.min(here), max.max(here));
        let margin = Vec3::splat(OCCLUSION_MARGIN);
        let (min, max) = (to_bevy((min - margin).to_array()), to_bevy((max + margin).to_array()));
        writes.commands.entity(p.parent).insert(ZoneCulled { zones: p2_collision::box_zones(model, min.into(), max.into()), min, max, only_from: None });
    }
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

fn upload_sprites(buffers: &mut Assets<ShaderBuffer>, records: &[[u32; RECORD_WORDS]]) {
    let Some(mut buffer) = buffers.get_mut(PARTICLE_SPRITES.id()) else { return };
    let record_bytes = (RECORD_WORDS * 4) as u64;
    let capacity = grid_capacity(u32::try_from(buffer.buffer_size() / record_bytes).unwrap_or(u32::MAX), records.len());
    buffer.clear();
    buffer.extend_from_slice(records);
    buffer.resize_buffer(u64::from(capacity) * record_bytes);
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
mod tests {
    use super::*;

    fn to_linear(x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    }

    fn to_display(x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
    }

    /// `s`: the texel × vertex colour in display space; `a`: the alpha; `d`: the framebuffer's display brightness. The
    /// framebuffer blends in linear light.
    fn drawn(style: DrawStyle, s: f32, a: f32, d: f32) -> f32 {
        let (b, dl) = (to_linear(0.25), to_linear(d));
        let over = |source: f32, keep: f32| (to_linear(source + 0.25 * keep) - b * keep).max(0.0);
        let out = match style {
            DrawStyle::Translucent => over(s, 1.0) + dl,
            DrawStyle::Brighten => {
                let f = ((to_linear(s + 0.25 * (1.0 - s)) - b) / (1.0 - b)).max(0.0);
                f + dl * (1.0 - f)
            }
            DrawStyle::AlphaBlend => over(s * a, 1.0 - a) + dl * (1.0 - a),
            DrawStyle::AlphaModulate => over(s, 1.0 - a) + dl * (1.0 - a),
            DrawStyle::Darken => dl * to_linear(1.0 - s),
            DrawStyle::Modulated => 2.0 * ((2.0 * s).powf(2.2) * 0.5).min(1.0) * dl,
            DrawStyle::Regular => to_linear(s),
        };
        to_display(out.min(1.0))
    }

    /// What the engine shows (D3D blending in display space, saturating).
    fn engine(style: DrawStyle, s: f32, a: f32, d: f32) -> f32 {
        let out = match style {
            DrawStyle::Translucent => s + d,
            DrawStyle::Brighten => s + d * (1.0 - s),
            DrawStyle::AlphaBlend => s * a + d * (1.0 - a),
            DrawStyle::AlphaModulate => s + d * (1.0 - a),
            DrawStyle::Darken => d * (1.0 - s),
            DrawStyle::Modulated => 2.0 * s * d,
            DrawStyle::Regular => s,
        };
        out.clamp(0.0, 1.0)
    }

    fn before(s: f32, a: f32, d: f32) -> f32 {
        to_display(to_linear(d) + to_linear(s) * a)
    }

    #[test]
    fn fire_sprites_are_as_bright_as_the_engines() {
        let flame = [1.0, 0.6, 0.2];
        let texture_alpha = 0.4;
        for style in [DrawStyle::Translucent, DrawStyle::Brighten] {
            for d in [0.0, 0.25, 0.35, 0.45, 0.6] {
                let engine_rgb = flame.map(|s| engine(style, s, texture_alpha, d));
                let now_rgb = flame.map(|s| drawn(style, s, texture_alpha, d));
                let before_rgb = flame.map(|s| before(s, texture_alpha, d));
                eprintln!("{style:?} over {d:.2}: engine {engine_rgb:.2?}, now {now_rgb:.2?}, before {before_rgb:.2?}");
                for c in 0..3 {
                    let (now, old) = ((now_rgb[c] - engine_rgb[c]).abs(), (before_rgb[c] - engine_rgb[c]).abs());
                    if d == 0.25 {
                        assert!(now < 0.005, "{style:?} over {d}: {now_rgb:?} vs {engine_rgb:?}");
                    }
                    if d > 0.0 {
                        assert!(now <= old + 1e-4, "{style:?} over {d}, channel {c}: now {now} worse than before {old}");
                    }
                    if (0.25..=0.6).contains(&d) {
                        assert!(now < 0.12, "{style:?} over {d}: {now_rgb:?} vs {engine_rgb:?}");
                    }
                }
            }
        }
        for s in [0.1, 0.5, 0.9] {
            for d in [0.3, 0.5, 0.9] {
                let (now, then) = (drawn(DrawStyle::Darken, s, 1.0, d), engine(DrawStyle::Darken, s, 1.0, d));
                assert!((now - then).abs() < 0.02 || then < 0.1, "darken {s} over {d}: {now} vs {then}");
            }
            for style in [DrawStyle::AlphaBlend, DrawStyle::AlphaModulate] {
                assert!((drawn(style, s, 0.5, 0.25) - engine(style, s, 0.5, 0.25)).abs() < 0.005, "{style:?} {s}");
            }
        }
    }

    #[test]
    fn padded_emitter_meshes_sort_by_the_actors_centre() {
        let sprite = |x: f32| SpriteParticle {
            location: [x, 0.0, 0.0].into(),
            size: [2.0; 2],
            up: [0.0, 0.0, 2.0].into(),
            right: [0.0, 2.0, 0.0].into(),
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
        };
        let mut meshes = [sprite(0.0), sprite(100.0)].map(|s| {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            fill_sprites(&mut mesh, &[s]);
            mesh
        });
        let own: Vec<Vec3> = meshes.iter().map(|m| Vec3::from(m.get_aabb().expect("bounds").center)).collect();
        assert_ne!(own[0], own[1]);
        let (min, max) = (Vec3::new(-2.0, -2.0, -2.0), Vec3::new(102.0, 2.0, 2.0));
        for mesh in &mut meshes {
            pad_bounds(mesh, min, max);
            assert_eq!(Vec3::from(mesh.get_aabb().expect("bounds").center), (min + max) / 2.0);
            assert_eq!(mesh.count_vertices(), 6);
            assert!(mesh.indices().expect("indices").iter().all(|i| i < 4), "the padding is drawn");
        }
        assert!(emitter_sort_bias(1) > emitter_sort_bias(0));
    }

    #[test]
    fn sprite_boxes_are_the_meshes_bounds() {
        let sprite = |location: Vector, up: Vector, right: Vector| SpriteParticle {
            location,
            size: [2.0; 2],
            up,
            right,
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
        };
        let sprites = [sprite([0.0; 3].into(), [0.0, 0.0, 2.0].into(), [0.0, 2.0, 0.0].into()), sprite([50.0, -20.0, 7.0].into(), [1.0, 1.0, 3.0].into(), [-2.0, 1.5, 0.0].into())];
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        fill_sprites(&mut mesh, &sprites);
        let aabb = mesh.get_aabb().expect("bounds");
        let (min, max) = sprites.iter().map(sprite_box).reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max))).expect("particles");
        assert!(min.abs_diff_eq(Vec3::from(aabb.min()), 1e-4) && max.abs_diff_eq(Vec3::from(aabb.max()), 1e-4), "{min} {max} vs {aabb:?}");
    }

    #[test]
    fn floor_sprites_are_not_occluded_by_their_floor() {
        let puddle = SpriteParticle {
            location: [10.0, 20.0, -0.3].into(),
            size: [30.0; 2],
            up: [30.0, 0.0, 0.0].into(),
            right: [0.0, 30.0, 0.0].into(),
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
        };
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        fill_sprites(&mut mesh, &[puddle]);
        let own = mesh.get_aabb().expect("bounds");
        let (min, max) = (Vec3::from(own.min()), Vec3::from(own.max()));
        assert_eq!((min.y, max.y), (-0.3, -0.3), "flat, under the floor");
        let culled = occlusion_aabb(min, max);
        assert!(culled.max().y > 1.0, "{culled:?}");
        assert!(Vec3::from(culled.center).abs_diff_eq((min + max) / 2.0, 1e-4), "{culled:?}");
        assert!(culled.min().x < -19.0 && culled.max().z > 49.0, "the puddle's extent in Bevy X / Z: {culled:?}");
    }

    fn shown_frames(blend_between_subdivisions: bool) -> Vec<f32> {
        use bevy::mesh::VertexAttributeValues;
        use p2_particles::{EmitterProperties, EmitterSettings, Range};
        let props = EmitterProperties {
            max_particles: 1,
            respawn_dead_particles: false,
            initial_particles_per_second: 0.0,
            particles_per_second: 0.0,
            automatic_initial_spawning: false,
            lifetime_range: Range::new(0.6, 0.6),
            texture_u_subdivisions: 1,
            texture_v_subdivisions: 4,
            blend_between_subdivisions,
            ..EmitterProperties::default()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        emitter.tick(1.0 / 60.0, [0.0; 3].into(), [0; 3].into());
        emitter.spawn_one(0, [0.0; 3].into());
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let mut shown = Vec::new();
        for _ in 0..27 {
            emitter.tick(1.0 / 60.0, [0.0; 3].into(), [0; 3].into());
            let sprites: Vec<SpriteParticle> = emitter.sprites(0, p2_particles::View::default()).collect();
            fill_sprites(&mut mesh, &sprites);
            let (
                Some(VertexAttributeValues::Float32x2(uvs)),
                Some(VertexAttributeValues::Float32x2(next)),
                Some(VertexAttributeValues::Float32x4(colors)),
            ) = (mesh.attribute(Mesh::ATTRIBUTE_UV_0), mesh.attribute(Mesh::ATTRIBUTE_UV_1), mesh.attribute(Mesh::ATTRIBUTE_COLOR))
            else {
                panic!("no uvs or colours");
            };
            let frame = |uv: &[f32; 2]| uv[1] / 0.25;
            let blend = if blend_between_subdivisions { colors[0][3] } else { 0.0 };
            shown.push(frame(&uvs[0]) * (1.0 - blend) + frame(&next[0]) * blend);
        }
        shown
    }

    #[test]
    fn flipbook_particles_advance_every_frame() {
        let step = 4.0 / 0.6 / 60.0;
        let blended = shown_frames(true);
        for (frame, pair) in blended.windows(2).enumerate() {
            assert!((pair[1] - pair[0] - step).abs() < 0.01, "frame {frame}: {blended:?}");
        }
        let plain = shown_frames(false);
        assert!(plain.windows(2).any(|pair| pair[1] - pair[0] > 0.9), "{plain:?}");
        assert!(plain.windows(2).filter(|pair| pair[1] == pair[0]).count() > 20, "{plain:?}");
    }

    #[test]
    fn sprite_records_expand_to_the_cpu_quads() {
        use bevy::mesh::VertexAttributeValues::{Float32x2, Float32x3, Float32x4};
        let sprite = |blend| SpriteParticle {
            location: [10.0, -4.0, 7.0].into(),
            size: [2.0; 2],
            up: [0.5, 1.0, 3.0].into(),
            right: [-2.0, 1.5, 0.25].into(),
            color: [10, 128, 200, 77],
            uv: [0.25, 0.5, 0.5, 0.75],
            blend,
        };
        for sprite in [sprite(None), sprite(Some(([0.5, 0.5, 0.75, 0.75], 200)))] {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            fill_sprites(&mut mesh, std::slice::from_ref(&sprite));
            let record = sprite_record(&sprite, Vec3::ZERO);
            let float = |i: usize| f32::from_bits(record[i]);
            let vec = |i: usize| Vec3::new(float(i), float(i + 1), float(i + 2));
            let (center, up, right) = (vec(0), vec(4), vec(8));
            let color = record[3].to_le_bytes().map(|c| f32::from(c) / 255.0);
            let (Some(Float32x3(positions)), Some(Float32x2(uvs)), Some(Float32x2(next)), Some(Float32x4(colors))) = (
                mesh.attribute(Mesh::ATTRIBUTE_POSITION),
                mesh.attribute(Mesh::ATTRIBUTE_UV_0),
                mesh.attribute(Mesh::ATTRIBUTE_UV_1),
                mesh.attribute(Mesh::ATTRIBUTE_COLOR),
            ) else {
                panic!("attributes")
            };
            for corner in 0..4 {
                let (along_right, along_up) = (corner == 1 || corner == 2, corner >= 2);
                let at = center + if along_up { up } else { -up } + if along_right { right } else { -right };
                assert!(at.abs_diff_eq(Vec3::from(positions[corner]), 1e-5), "corner {corner}: {at} vs {:?}", positions[corner]);
                let rect = |i: usize| [if along_right { float(i + 2) } else { float(i) }, if along_up { float(i + 3) } else { float(i + 1) }];
                assert_eq!(rect(12), uvs[corner], "corner {corner}");
                assert_eq!(rect(16), next[corner], "corner {corner}");
                assert_eq!(color, colors[corner], "corner {corner}");
            }
        }
        assert_eq!(header_record(5)[7], 5);
        assert!(header_record(5).iter().enumerate().all(|(i, &w)| i == 7 || w == 0));
    }

    #[test]
    fn sprite_grids_sort_at_the_actors_box_centre() {
        let (min, max) = (Vec3::new(100.0, -20.0, 7.0), Vec3::new(300.0, 40.0, 9.0));
        let (place, aabb) = grid_place(min, max);
        for quads in [MIN_QUADS, 64] {
            let sort_centre = place.transform_point(Vec3::from(sprite_grid(quads).get_aabb().expect("bounds").center));
            assert_eq!(sort_centre, (min + max) / 2.0);
        }
        let whole = occlusion_aabb(min, max);
        assert_eq!(place.transform_point(Vec3::from(aabb.center)), Vec3::from(whole.center));
        assert_eq!(aabb.half_extents, whole.half_extents);
    }

    #[test]
    fn sprite_records_keep_their_place_on_a_moved_grid() {
        let location: Vector = [50.0, -20.0, 7.0].into();
        let sprite = SpriteParticle { location, size: [2.0; 2], up: [0.0; 3].into(), right: [0.0; 3].into(), color: [255; 4], uv: [0.0; 4], blend: None };
        let grid_at = Vec3::new(40.0, 3.0, -9.0);
        let record = sprite_record(&sprite, grid_at);
        let centre = Vec3::new(f32::from_bits(record[0]), f32::from_bits(record[1]), f32::from_bits(record[2]));
        assert_eq!(centre + grid_at, bevy_vector(location));
    }

    #[test]
    fn sprite_grids_grow_by_powers_of_two() {
        assert_eq!(grid_capacity(MIN_QUADS, 3), MIN_QUADS);
        assert_eq!(grid_capacity(MIN_QUADS, 16), 16);
        assert_eq!(grid_capacity(MIN_QUADS, 17), 32);
        assert_eq!(grid_capacity(64, 2), 64);
        assert_eq!(grid_capacity(1, 1), 1);
        assert_eq!(grid_capacity(1, 2), MIN_QUADS);
        assert_eq!(grid_capacity(32, 1000), 1024);
        let grid = sprite_grid(3);
        assert_eq!(grid.count_vertices(), 12);
        assert!(grid.attribute(Mesh::ATTRIBUTE_COLOR).is_none(), "told from CPU meshes by its missing colours");
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) = grid.attribute(Mesh::ATTRIBUTE_POSITION) else { panic!("positions") };
        assert_eq!(positions[9], [2.0, 1.0, 0.0], "quad 2, corner 1");
        let indices: Vec<usize> = grid.indices().expect("indices").iter().collect();
        assert_eq!(indices[12..18], [8, 9, 10, 8, 10, 11]);
    }
}
