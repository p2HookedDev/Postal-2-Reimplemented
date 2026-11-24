//! Modern graphics: rigid pieces. The shards of a break, the pieces of a head's explosion and the sparks of an impact
//! are thrown as rigid bodies that land on the level around them. A shard or a lump falls asleep and lies until a
//! blast moves it again; a spark bounces and burns out. Pieces share small physics worlds by place; nothing reads them back.

use std::f32::consts::TAU;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Asset, AssetId, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::color::{Alpha, Color, ColorToComponents};
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::NotShadowCaster;
use bevy::material::AlphaMode;
use bevy::math::{EulerRot, Mat3, Mat4, Quat, Vec3};
use bevy::mesh::{Indices, Mesh, Mesh3d, MeshTag, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin, MeshMaterial3d, StandardMaterial};
use bevy::reflect::Reflect;
use bevy::render::RenderApp;
use bevy::render::render_resource::AsBindGroup;
use bevy::render::storage::ShaderBuffer;
use bevy::shader::{Shader, ShaderRef};
use bevy::time::Time;
use bevy::transform::components::Transform;
use p2_bevy_assets::{to_bevy, unreal_to_bevy};
use p2_host::display::{MODERN_GPU_PARTICLES, Settings, modern_on, modern_turned_off};
use p2_math::Vector;
use p2_physics::{BodyId, BodyParams, Contacts, Shape, World};
use rustc_hash::FxHashMap;

use crate::frame_facts::FrameFacts;
use crate::gpu_particles::GpuParticleSystems;
use crate::level_collision::{LevelCollision, LevelTriangles};
use crate::particle_draw::ParticleDrawSystems;
use crate::particle_frame::{Blast, DrawnParticles};
use crate::particle_throw::next_unit;
use crate::shaders::{add_shaders, shader_file};
use crate::slot_buffer::SlotBuffer;

const PIECES_PER_LEVEL: usize = 64;
/// The tint buffer's first size: the lying pieces' cap and the sparks' cap at the highest setting. Pieces fading out to
/// make room count against neither, so it may grow.
const SLOTS: usize = 2 * 10 * PIECES_PER_LEVEL;
const DEBRIS_SHADER: Handle<Shader> = uuid_handle!("9c4e1a72-3d58-4b06-a8f1-6e27d0b5c934");
/// Every piece's colour in linear light, at its `MeshTag`.
const DEBRIS_TINTS: Handle<ShaderBuffer> = uuid_handle!("e5a03f81-7b6c-4d29-9e14-c83f20a6d7b5");
/// One break or one head takes at most this share of the lying pieces' cap, so several lie at once.
const BREAK_SHARE: usize = 4;
const WORLDS: usize = 8;
/// Half the side of the box of level a world holds, in Unreal units.
const REACH: f32 = 500.0;
/// A piece joins a world only this far inside its box, in Unreal units, so it never starts at the box's edge.
const ROOM: f32 = 200.0;
/// Half a piece's thickness, in Unreal units: thick enough not to pass a floor between two steps.
const THICKNESS: f32 = 2.0;
/// A lump's box against the quads it is drawn with: the texture's corners are empty.
const LUMP_BODY: f32 = 0.6;
const SPARK_RADIUS: f32 = 1.0;
const GRAVITY: f32 = 950.0;
/// Radians a second, most, about each axis.
const SPIN: f32 = 12.0;
/// A piece slower than this, in Unreal units a second and radians a second, for the seconds given falls asleep.
const SLEEP: [f32; 3] = [15.0, 0.8, 0.3];
const AWAKE_MOST: f32 = 5.0;
/// Pieces of one emitter actor thrown within this many seconds of each other share a group.
const TOGETHER: f32 = 0.25;
const FADE: f32 = 3.0;
const FRAME_MOST: f32 = 0.1;
/// A spark's streak is this many times as long as it is wide at `STREAK_SPEED` Unreal units a second and faster.
const STREAK: f32 = 12.0;
const STREAK_SPEED: f32 = 300.0;
/// The colour a spark burns to.
const EMBER: Vec3 = Vec3::new(1.0, 0.15, 0.05);

pub struct DebrisPlugin;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct DebrisSystems;

impl Plugin for DebrisPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(DEBRIS_SHADER, shader_file("debris.wesl"))]);
        let mut debris = Debris::default();
        debris.loose.tints.upload(&mut app.world_mut().resource_mut::<Assets<ShaderBuffer>>());
        app.add_plugins(MaterialPlugin::<ExtendedMaterial<StandardMaterial, DebrisTint>>::default());
        // After the droplets' thrower: it hands over this frame's pieces, and the hits' pushes come before it.
        app.init_resource::<PieceBirths>().insert_resource(debris).add_systems(
            PostUpdate,
            (
                (start_pieces.run_if(modern_on(MODERN_GPU_PARTICLES)), (|mut debris: ResMut<Debris>| debris.clear()).run_if(modern_turned_off(MODERN_GPU_PARTICLES))),
                move_pieces,
            )
                .chain().in_set(DebrisSystems).in_set(ParticleDrawSystems).after(GpuParticleSystems),
        );
    }
}

#[derive(Asset, AsBindGroup, Reflect, Clone)]
struct DebrisTint {
    #[storage(100, read_only)]
    tints: Handle<ShaderBuffer>,
}

impl MaterialExtension for DebrisTint {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(DEBRIS_SHADER)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PieceKind {
    /// A thin box that lies where it lands: a shard.
    Flat,
    /// A box as high as it is wide that bounces and rolls, drawn as three crossed quads: a piece of skull or brain.
    Lump,
    /// A small ball that bounces and burns out, drawn as a streak along its motion.
    Spark,
}

struct PieceTraits {
    /// The body for a piece of this half size.
    shape: fn(f32) -> Shape,
    friction: f32,
    restitution: f32,
    alpha_mode: AlphaMode,
    mesh: fn([f32; 4]) -> Mesh,
    /// Thrown at a random rotation, else unturned.
    tumbles: bool,
}

impl PieceKind {
    fn traits(self) -> PieceTraits {
        match self {
            Self::Flat => PieceTraits {
                shape: |half| {
                    let side = half.max(THICKNESS);
                    Shape::Box(Vector::new(side, side, THICKNESS))
                },
                friction: 0.7,
                restitution: 0.15,
                alpha_mode: AlphaMode::Blend,
                mesh: quad,
                tumbles: true,
            },
            Self::Lump => PieceTraits {
                shape: |half| {
                    let side = (half * LUMP_BODY).max(THICKNESS);
                    Shape::Box(Vector::new(side, side, side))
                },
                friction: 0.6,
                restitution: 0.35,
                alpha_mode: AlphaMode::Blend,
                mesh: crossed_quads,
                tumbles: true,
            },
            Self::Spark => PieceTraits {
                shape: |_| Shape::Sphere(Vector::ZERO, SPARK_RADIUS),
                friction: 0.1,
                restitution: 0.5,
                alpha_mode: AlphaMode::Add,
                mesh: quad,
                tumbles: false,
            },
        }
    }
}

/// A piece a sprite throws, in Unreal units and axes.
pub(crate) struct PieceBirth {
    pub(crate) kind: PieceKind,
    pub(crate) place: Vector,
    pub(crate) velocity: Vector,
    pub(crate) half: f32,
    pub(crate) uv: [f32; 4],
    pub(crate) image: Handle<Image>,
    /// In display light.
    pub(crate) tint: Vec3,
    pub(crate) parent: Entity,
    /// How long the piece's group lives.
    pub(crate) seconds: f32,
}

#[derive(Resource, Default)]
pub(crate) struct PieceBirths(pub(crate) Vec<PieceBirth>);

#[derive(Component)]
struct DebrisPiece;

#[derive(Clone, Copy)]
struct Piece {
    entity: Entity,
    /// The world and body while it moves; `None` while it lies.
    body: Option<(u32, BodyId)>,
    place: Vector,
    rotation: [f32; 4],
    velocity: Vector,
    half: f32,
    /// In display light.
    tint: Vec3,
    slot: u32,
    /// The group's age when the piece was last thrown or pushed.
    woke: f32,
    /// Moved since it was last drawn.
    stirred: bool,
}

/// Pieces thrown together: one age.
struct Group {
    parent: Entity,
    kind: PieceKind,
    pieces: Vec<Piece>,
    age: f32,
    seconds: f32,
    /// Fading out early to make room; its pieces no longer count against the cap.
    ending: bool,
}

/// A small world of the level within `REACH` of `centre`.
struct Place {
    id: u32,
    centre: Vector,
    world: World,
}

struct Loose {
    /// Shards and lumps, oldest first.
    lying: Vec<Group>,
    /// Oldest first; a spark without a body is gone.
    sparks: Vec<Group>,
    /// Oldest first.
    worlds: Vec<Place>,
    serial: u32,
    seed: u32,
    /// The entities of pieces that went, for the systems to despawn.
    gone: Vec<Entity>,
    tints: SlotBuffer<[f32; 4]>,
}

impl Default for Loose {
    fn default() -> Self {
        Self { lying: Vec::new(), sparks: Vec::new(), worlds: Vec::new(), serial: 0, seed: 0, gone: Vec::new(), tints: SlotBuffer::new(DEBRIS_TINTS, SLOTS) }
    }
}

fn world_mut(worlds: &mut [Place], id: u32) -> Option<&mut World> {
    worlds.iter_mut().find(|place| place.id == id).map(|place| &mut place.world)
}

fn in_world(body: Option<(u32, BodyId)>, id: u32) -> bool {
    body.is_some_and(|(world, _)| world == id)
}

fn joined(groups: &[Group], parent: Entity, kind: PieceKind) -> Option<usize> {
    groups.iter().rposition(|held| held.parent == parent && held.kind == kind && !held.ending && held.age < TOGETHER)
}

/// Makes room under `cap` in `groups` for one more piece of `kind` thrown by `parent`; false when the piece is not
/// thrown. The oldest groups end early and stop counting.
fn room_under_cap(groups: &mut [Group], parent: Entity, kind: PieceKind, cap: usize) -> bool {
    let (counted, sizes): (Vec<usize>, Vec<usize>) = groups.iter().enumerate().filter(|(_, held)| !held.ending).map(|(at, held)| (at, held.pieces.len())).unzip();
    if sizes.iter().sum::<usize>() < cap {
        return true;
    }
    let joins = joined(groups, parent, kind).and_then(|group| counted.iter().position(|&at| at == group));
    let Some(dropped) = groups_to_drop(&sizes, joins, cap) else { return false };
    for &at in &counted[..dropped] {
        let held = &mut groups[at];
        held.ending = true;
        held.seconds = held.seconds.min(held.age + FADE);
    }
    true
}

impl Loose {
    fn list_mut(&mut self, kind: PieceKind) -> &mut Vec<Group> {
        match kind {
            PieceKind::Flat | PieceKind::Lump => &mut self.lying,
            PieceKind::Spark => &mut self.sparks,
        }
    }

    fn end(&mut self, groups: impl IntoIterator<Item = Group>) {
        for piece in groups.into_iter().flat_map(|group| group.pieces) {
            if let Some((id, body)) = piece.body
                && let Some(world) = world_mut(&mut self.worlds, id)
            {
                world.remove_body(body);
            }
            self.gone.push(piece.entity);
            self.tints.free(piece.slot);
        }
    }

    /// Sparks have a cap of their own, so a burst of fire never takes a lying piece's place. The oldest sparks go at
    /// once; the oldest lying pieces fade out.
    fn make_room(&mut self, parent: Entity, kind: PieceKind, cap: usize) -> bool {
        match kind {
            PieceKind::Spark => {
                let room = room_under_cap(&mut self.sparks, parent, kind, cap);
                let ended: Vec<Group> = self.sparks.extract_if(.., |group| group.ending).collect();
                self.end(ended);
                room
            }
            PieceKind::Flat | PieceKind::Lump => {
                let own: usize = self.lying.iter().filter(|held| !held.ending && held.parent == parent).map(|held| held.pieces.len()).sum();
                own < cap / BREAK_SHARE && room_under_cap(&mut self.lying, parent, kind, cap)
            }
        }
    }

    /// A world goes: the oldest in which no shard or lump still flies, else the oldest. What still flies in it goes
    /// rather than hang in the air.
    fn retire_a_world(&mut self) {
        let flying = |id: u32| self.lying.iter().flat_map(|group| &group.pieces).any(|piece| in_world(piece.body, id));
        let Some(id) = self.worlds.iter().map(|place| place.id).find(|&id| !flying(id)).or(self.worlds.first().map(|place| place.id)) else { return };
        for group in self.lying.iter_mut().chain(&mut self.sparks) {
            for piece in group.pieces.extract_if(.., |piece| in_world(piece.body, id)) {
                self.gone.push(piece.entity);
                self.tints.free(piece.slot);
            }
        }
        self.worlds.retain(|place| place.id != id);
    }

    /// A live world whose box holds `at` with room to spare, else a new one around it.
    fn world_holding(&mut self, at: Vector, level: impl FnOnce(Vector) -> Vec<[[f32; 3]; 3]>) -> u32 {
        if let Some(place) = self.worlds.iter().rev().find(|place| <[f32; 3]>::from(at - place.centre).iter().all(|k| k.abs() <= REACH - ROOM)) {
            return place.id;
        }
        if self.worlds.len() >= WORLDS {
            self.retire_a_world();
        }
        self.serial += 1;
        self.worlds.push(Place { id: self.serial, centre: at, world: world_around(level(at)) });
        self.serial
    }

    fn random_spin(&mut self) -> Vector {
        let mut unit = || next_unit(&mut self.seed) * 2.0 - 1.0;
        Vector::new(unit(), unit(), unit()) * SPIN
    }

    fn random_rotation(&mut self) -> [f32; 4] {
        let mut unit = || next_unit(&mut self.seed) * TAU;
        Quat::from_euler(EulerRot::XYZ, unit(), unit(), unit()).to_array()
    }

    /// A lying piece gets a body again in a world that holds it.
    fn blast(&mut self, blast: &Blast, level: impl Fn(Vector) -> Vec<[[f32; 3]; 3]>) {
        let mut hits = Vec::new();
        for list in [&self.lying, &self.sparks] {
            for (g, group) in list.iter().enumerate() {
                for piece in group.pieces.iter().filter(|piece| (piece.place - blast.at).size() < blast.radius) {
                    hits.push((group.kind, g, piece.entity));
                }
            }
        }
        let up = Vector::new(0.0, 0.0, 1.0);
        for (kind, g, entity) in hits {
            // Found by its entity again: a world made for an earlier hit may have retired this piece's world or taken
            // a flying piece out of its group.
            let Some((age, piece)) = self.list_mut(kind).get(g).and_then(|group| Some((group.age, *group.pieces.iter().find(|piece| piece.entity == entity)?))) else { continue };
            let off = piece.place - blast.at;
            let distance = off.size();
            let away = if distance > 1e-3 { off / distance } else { up };
            let lift = away + blast.way + up;
            let share = 1.0 - distance / blast.radius;
            let change = lift / lift.size() * (blast.push * share);
            let spin = self.random_spin() * share;
            let body = match piece.body {
                Some((id, body)) => {
                    let Some(world) = world_mut(&mut self.worlds, id) else { continue };
                    let velocity = world.velocity(body).unwrap_or(Vector::ZERO) + change;
                    world.set_velocity(body, velocity);
                    world.set_spin(body, spin);
                    (id, body)
                }
                None => {
                    let id = self.world_holding(piece.place, &level);
                    let Some(world) = world_mut(&mut self.worlds, id) else { continue };
                    (id, add_piece(world, kind, &piece, change, spin))
                }
            };
            if let Some(held) = self.list_mut(kind).get_mut(g).and_then(|group| group.pieces.iter_mut().find(|held| held.entity == entity)) {
                (held.body, held.woke) = (Some(body), age);
            }
        }
    }

    /// A piece is stepped until it sleeps or has been awake too long; then its body goes. A shard or a lump lies where
    /// it is, a spark is gone. A world with no body left is dropped.
    fn advance(&mut self, seconds: f32) {
        for place in &mut self.worlds {
            place.world.step(seconds);
        }
        for group in self.lying.iter_mut().chain(&mut self.sparks) {
            group.age += seconds;
            for piece in &mut group.pieces {
                let Some((id, body)) = piece.body else { continue };
                let Some(world) = world_mut(&mut self.worlds, id) else {
                    piece.body = None;
                    continue;
                };
                if let Some((at, rotation)) = world.pose(body) {
                    (piece.place, piece.rotation) = (at, rotation);
                }
                piece.velocity = world.velocity(body).unwrap_or(Vector::ZERO);
                piece.stirred = true;
                if world.is_sleeping(body) || group.age - piece.woke > AWAKE_MOST {
                    world.remove_body(body);
                    piece.body = None;
                }
            }
        }
        for group in &mut self.sparks {
            for piece in group.pieces.extract_if(.., |piece| piece.body.is_none()) {
                self.gone.push(piece.entity);
                self.tints.free(piece.slot);
            }
        }
        let over = |group: &mut Group| group.age >= group.seconds || group.pieces.is_empty();
        let ended: Vec<Group> = self.lying.extract_if(.., over).chain(self.sparks.extract_if(.., over)).collect();
        self.end(ended);
        self.worlds.retain(|place| self.lying.iter().chain(&self.sparks).flat_map(|group| &group.pieces).any(|piece| in_world(piece.body, place.id)));
    }
}

#[derive(Resource, Default)]
struct Debris {
    map: u32,
    loose: Loose,
    triangles: Option<LevelTriangles>,
    meshes: FxHashMap<(PieceKind, [u32; 4]), Handle<Mesh>>,
    /// By texture and whether it adds.
    materials: FxHashMap<(AssetId<Image>, bool), Handle<ExtendedMaterial<StandardMaterial, DebrisTint>>>,
    origin: Vec3,
    /// Bevy axes relative to the world's origin.
    eye: Vec3,
}

impl Debris {
    fn clear(&mut self) {
        let lying = std::mem::take(&mut self.loose.lying);
        let sparks = std::mem::take(&mut self.loose.sparks);
        self.loose.end(lying.into_iter().chain(sparks));
        self.loose.worlds.clear();
        self.triangles = None;
        self.meshes.clear();
        self.materials.clear();
    }

    fn add(
        &mut self,
        commands: &mut Commands,
        birth: &PieceBirth,
        cap: usize,
        origin: Vec3,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<ExtendedMaterial<StandardMaterial, DebrisTint>>,
    ) {
        let Self { loose, triangles, meshes: held_meshes, materials: held_materials, eye, .. } = self;
        let Some(triangles) = triangles.as_ref() else { return };
        if !loose.make_room(birth.parent, birth.kind, cap) {
            return;
        }
        let traits = birth.kind.traits();
        let slot = loose.tints.take();
        let world = loose.world_holding(birth.place, |centre| triangles.near(centre, REACH));
        let groups = loose.list_mut(birth.kind);
        let at = joined(groups, birth.parent, birth.kind).unwrap_or_else(|| {
            groups.push(Group { parent: birth.parent, kind: birth.kind, pieces: Vec::new(), age: 0.0, seconds: birth.seconds, ending: false });
            groups.len() - 1
        });
        let age = groups[at].age;
        let material = held_materials
            .entry((birth.image.id(), traits.alpha_mode == AlphaMode::Add))
            .or_insert_with(|| {
                materials.add(ExtendedMaterial {
                    base: StandardMaterial {
                        base_color_texture: Some(birth.image.clone()),
                        alpha_mode: traits.alpha_mode,
                        unlit: true,
                        double_sided: true,
                        cull_mode: None,
                        ..Default::default()
                    },
                    extension: DebrisTint { tints: DEBRIS_TINTS },
                })
            })
            .clone();
        let rotation = if traits.tumbles { loose.random_rotation() } else { [0.0, 0.0, 0.0, 1.0] };
        let spin = loose.random_spin();
        let mesh = held_meshes.entry((birth.kind, birth.uv.map(f32::to_bits))).or_insert_with(|| meshes.add((traits.mesh)(birth.uv))).clone();
        let Some(held) = world_mut(&mut loose.worlds, world) else {
            loose.tints.free(slot);
            return;
        };
        let mut piece = Piece {
            entity: Entity::PLACEHOLDER,
            body: None,
            place: birth.place,
            rotation,
            velocity: birth.velocity,
            half: birth.half,
            tint: birth.tint,
            slot,
            woke: age,
            stirred: false,
        };
        piece.body = Some((world, add_piece(held, birth.kind, &piece, birth.velocity, spin)));
        let transform = match birth.kind {
            PieceKind::Flat | PieceKind::Lump => piece_transform(&piece, birth.kind, origin),
            PieceKind::Spark => spark_transform(&piece, *eye, origin),
        };
        piece.entity = commands.spawn((Mesh3d(mesh), MeshMaterial3d(material), MeshTag::new(slot), transform, NotShadowCaster, DebrisPiece)).id();
        if let Some(group) = loose.list_mut(birth.kind).get_mut(at) {
            group.pieces.push(piece);
        }
    }
}

fn piece_cap(level: u8) -> usize {
    PIECES_PER_LEVEL * usize::from(level.min(10))
}

/// How many of the oldest groups go so that one more piece fits under `cap`; `None` when the group the piece joins
/// would have to go.
fn groups_to_drop(sizes: &[usize], joins: Option<usize>, cap: usize) -> Option<usize> {
    let mut total: usize = sizes.iter().sum();
    let mut dropped = 0;
    while total >= cap {
        if joins == Some(dropped) {
            return None;
        }
        total -= sizes.get(dropped)?;
        dropped += 1;
    }
    Some(dropped)
}

/// The level is two-sided here: the BSP's triangles are not wound toward the open side.
fn world_around(triangles: Vec<[[f32; 3]; 3]>) -> World {
    let mut world = World::with_threads(Vector::new(0.0, 0.0, -GRAVITY), 1);
    if !triangles.is_empty() {
        world.add_static_shape(&Shape::Mesh(triangles), Vector::new(0.0, 0.0, 0.0), [0.0, 0.0, 0.0, 1.0]).ok();
    }
    world
}

fn add_piece(world: &mut World, kind: PieceKind, piece: &Piece, velocity: Vector, spin: Vector) -> BodyId {
    let traits = kind.traits();
    let body = world.add_body(&BodyParams {
        shapes: vec![(traits.shape)(piece.half)],
        location: piece.place,
        rotation: piece.rotation,
        mass: 0.0,
        linear_damping: 0.1,
        angular_damping: 0.8,
        friction: traits.friction,
        restitution: traits.restitution,
        asleep: false,
        stay_upright: false,
        allow_yaw: false,
    });
    world.set_velocity(body, velocity);
    world.set_spin(body, spin);
    world.set_contacts(body, Contacts::LevelOnly);
    let [speed, turn, seconds] = SLEEP;
    world.set_sleep(body, speed, turn, seconds);
    body
}

/// A square of the texture's rectangle in the plane a piece's box is flat in.
fn quad([u0, v0, u1, v1]: [f32; 4]) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0]]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[u0, v0], [u1, v0], [u1, v1], [u0, v1]]);
    mesh.insert_indices(Indices::U16(vec![0, 2, 1, 0, 3, 2]));
    mesh
}

/// The quad on each of the three planes through the middle, so a lump shows from every side.
fn crossed_quads(uv: [f32; 4]) -> Mesh {
    let mut mesh = quad(uv);
    let turned = [Quat::from_rotation_x(TAU / 4.0), Quat::from_rotation_z(TAU / 4.0)];
    for turn in turned {
        mesh.merge(&quad(uv).rotated_by(turn)).ok();
    }
    mesh
}

fn piece_transform(piece: &Piece, kind: PieceKind, origin: Vec3) -> Transform {
    let high = if kind == PieceKind::Lump { piece.half } else { 1.0 };
    let scale = Vec3::new(piece.half, piece.half, high);
    let place = Vec3::from_array(piece.place.into());
    let mut at = unreal_to_bevy(Mat4::from_scale_rotation_translation(scale, Quat::from_array(piece.rotation).normalize(), place));
    at.translation += origin;
    at
}

/// The quad stretched along the spark's velocity and turned about it to face `eye`, as long as the spark is fast.
fn spark_transform(piece: &Piece, eye: Vec3, origin: Vec3) -> Transform {
    let place = Vec3::from(to_bevy(piece.place));
    let velocity = Vec3::from(to_bevy(piece.velocity));
    let speed = velocity.length();
    let along = if speed > 1.0 { velocity / speed } else { Vec3::Y };
    let side = along.cross(eye - place).normalize_or(along.any_orthonormal_vector());
    let reach = piece.half * (STREAK * (speed / STREAK_SPEED).min(1.0)).max(1.0);
    Transform { translation: place + origin, rotation: Quat::from_mat3(&Mat3::from_cols(side, along.cross(side), along)), scale: Vec3::new(piece.half, 1.0, reach) }
}

/// `age` is the share of its life gone: from its own colour to red, and out over the second half.
fn spark_color(tint: Vec3, age: f32) -> Color {
    let late = ((age - 0.5) * 2.0).clamp(0.0, 1.0);
    let fade = 1.0 - late * late * (3.0 - 2.0 * late);
    let [r, g, b] = (tint.powf(2.2) * Vec3::ONE.lerp(EMBER, age.clamp(0.0, 1.0)) * fade).to_array();
    Color::linear_rgb(r, g, b)
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn start_pieces(
    mut commands: Commands,
    settings: Res<Settings>,
    drawn: Option<Res<DrawnParticles>>,
    level: Option<Res<LevelCollision>>,
    facts: Res<FrameFacts>,
    mut births: ResMut<PieceBirths>,
    mut debris: ResMut<Debris>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, DebrisTint>>>,
) {
    let setting = settings.modern_level(MODERN_GPU_PARTICLES);
    let map = drawn.as_deref().map_or(0, |drawn| drawn.level);
    let debris = &mut *debris;
    if map != debris.map {
        debris.clear();
        debris.map = map;
    }
    let born = std::mem::take(&mut births.0);
    if let Some(drawn) = drawn.as_deref() {
        debris.eye = drawn.eye;
    }
    if let Some(collision) = level.as_deref().and_then(|level| level.0.clone()) {
        // Taken when the map is first seen, not at its first break, so the break's frame is not the long one.
        if debris.triangles.as_ref().is_none_or(|held| !held.holds(&collision)) {
            debris.triangles = Some(LevelTriangles::new(collision));
        }
        let cap = piece_cap(setting);
        for birth in &born {
            debris.add(&mut commands, birth, cap, facts.origin, &mut meshes, &mut materials);
        }
        // After this frame's births: what a hit throws takes the hit too.
        if let Some(triangles) = debris.triangles.as_ref() {
            for blast in drawn.as_deref().map_or(&[][..], |drawn| &drawn.blasts) {
                debris.loose.blast(blast, |centre| triangles.near(centre, REACH));
            }
        }
    }
    for entity in debris.loose.gone.drain(..) {
        commands.entity(entity).despawn();
    }
}

fn move_pieces(
    mut commands: Commands,
    time: Res<Time>,
    facts: Res<FrameFacts>,
    mut debris: ResMut<Debris>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut placed: Query<&mut Transform, With<DebrisPiece>>,
) {
    let seconds = facts.capped_step(time.delta_secs(), FRAME_MOST);
    let debris = &mut *debris;
    let shifted = std::mem::replace(&mut debris.origin, facts.origin) != facts.origin;
    debris.loose.advance(seconds);
    for entity in debris.loose.gone.drain(..) {
        commands.entity(entity).despawn();
    }
    let mut place = |entity: Entity, at: Transform| {
        if let Ok(mut transform) = placed.get_mut(entity)
            && *transform != at
        {
            *transform = at;
        }
    };
    let Loose { lying, sparks, tints, .. } = &mut debris.loose;
    for group in lying.iter_mut() {
        let left = group.seconds - group.age;
        let alpha = if left < FADE { (left / FADE).max(0.0) } else { 1.0 };
        for piece in &mut group.pieces {
            if std::mem::take(&mut piece.stirred) || shifted {
                place(piece.entity, piece_transform(piece, group.kind, facts.origin));
            }
            tints.set(piece.slot, Color::srgb(piece.tint.x, piece.tint.y, piece.tint.z).with_alpha(alpha).to_linear().to_f32_array());
        }
    }
    for group in sparks.iter() {
        let age = group.age / group.seconds.max(1e-3);
        for piece in &group.pieces {
            place(piece.entity, spark_transform(piece, debris.eye, facts.origin));
            tints.set(piece.slot, spark_color(piece.tint, age).to_linear().to_f32_array());
        }
    }
    tints.upload(&mut buffers);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: [[[f32; 3]; 3]; 2] =
        [[[-1000.0, -1000.0, 0.0], [1000.0, -1000.0, 0.0], [1000.0, 1000.0, 0.0]], [[-1000.0, -1000.0, 0.0], [-1000.0, 1000.0, 0.0], [1000.0, 1000.0, 0.0]]];

    fn lying_group(kind: PieceKind, place: Vector, seconds: f32) -> Group {
        let piece = Piece {
            entity: Entity::PLACEHOLDER,
            body: None,
            place,
            rotation: [0.0, 0.0, 0.0, 1.0],
            velocity: Vector::ZERO,
            half: 4.0,
            tint: Vec3::ONE,
            slot: 0,
            woke: 0.0,
            stirred: false,
        };
        Group { parent: Entity::PLACEHOLDER, kind, pieces: vec![piece], age: 0.0, seconds, ending: false }
    }

    #[test]
    fn a_shard_and_a_lump_land_on_a_floor_and_fall_asleep() {
        for kind in [PieceKind::Flat, PieceKind::Lump] {
            let mut world = world_around(FLOOR.to_vec());
            let group = lying_group(kind, Vector::new(0.0, 0.0, 80.0), 30.0);
            let body = add_piece(&mut world, kind, &group.pieces[0], Vector::new(150.0, 0.0, 300.0), Vector::new(5.0, 3.0, 0.0));
            let asleep = (0..(AWAKE_MOST * 60.0) as usize).position(|_| {
                world.step(1.0 / 60.0);
                world.is_sleeping(body)
            });
            assert!(asleep.is_some(), "{kind:?} still awake after {AWAKE_MOST} seconds");
            let (at, _) = world.pose(body).unwrap();
            assert!(at.z > 0.0 && at.z < 4.0 * THICKNESS + 4.0, "{kind:?} lies at {at:?}");
        }
    }

    #[test]
    fn a_lump_is_drawn_on_three_planes() {
        assert_eq!(crossed_quads([0.0, 0.0, 1.0, 1.0]).count_vertices(), 12);
    }

    #[test]
    fn a_blast_wakes_a_lying_piece_that_lies_again_farther_off() {
        let mut loose = Loose::default();
        let before = Vector::new(0.0, 0.0, THICKNESS);
        let far = Vector::new(500.0, 0.0, THICKNESS);
        loose.lying.push(lying_group(PieceKind::Flat, before, 30.0));
        loose.lying.push(lying_group(PieceKind::Flat, far, 30.0));
        let blast = Blast { at: Vector::new(-20.0, 0.0, 0.0), radius: 48.0, push: 220.0, way: Vector::ZERO };
        loose.blast(&blast, |_| FLOOR.to_vec());
        assert!(loose.lying[0].pieces[0].body.is_some(), "the near piece has a body again");
        assert!(loose.lying[1].pieces[0].body.is_none() && loose.lying[1].pieces[0].place == far, "the far piece stays as it lay");
        assert_eq!(loose.worlds.len(), 1);
        let frozen = (0..240).position(|_| {
            loose.advance(1.0 / 60.0);
            loose.lying[0].pieces[0].body.is_none()
        });
        assert!(frozen.is_some(), "still awake after four seconds");
        let after = loose.lying[0].pieces[0].place;
        assert!((after - blast.at).size() > (before - blast.at).size(), "lies at {after:?}");
        assert!(loose.worlds.is_empty(), "a world with no body left is dropped");
        assert_eq!(loose.lying[1].pieces[0].place, far);
    }

    #[test]
    fn a_blast_with_nothing_near_makes_no_world() {
        let mut loose = Loose::default();
        loose.lying.push(lying_group(PieceKind::Flat, Vector::new(500.0, 0.0, THICKNESS), 30.0));
        loose.blast(&Blast { at: Vector::ZERO, radius: 48.0, push: 220.0, way: Vector::ZERO }, |_| FLOOR.to_vec());
        assert!(loose.worlds.is_empty());
        assert!(loose.lying[0].pieces[0].body.is_none());
    }

    #[test]
    fn a_spark_is_gone_when_its_life_is_over() {
        let mut loose = Loose::default();
        let mut group = lying_group(PieceKind::Spark, Vector::new(0.0, 0.0, 200.0), 1.0);
        let id = loose.world_holding(group.pieces[0].place, |_| FLOOR.to_vec());
        let body = add_piece(&mut loose.worlds[0].world, PieceKind::Spark, &group.pieces[0], Vector::new(100.0, 0.0, 0.0), Vector::ZERO);
        group.pieces[0].body = Some((id, body));
        loose.sparks.push(group);
        for _ in 0..30 {
            loose.advance(1.0 / 60.0);
        }
        assert_eq!(loose.sparks.len(), 1, "alive at half its life");
        for _ in 0..40 {
            loose.advance(1.0 / 60.0);
        }
        assert!(loose.sparks.is_empty());
        assert_eq!(loose.gone, vec![Entity::PLACEHOLDER]);
        assert!(loose.worlds.is_empty());
    }

    #[test]
    fn pieces_near_each_other_share_a_world() {
        let mut loose = Loose::default();
        let first = loose.world_holding(Vector::ZERO, |_| FLOOR.to_vec());
        assert_eq!(loose.world_holding(Vector::new(50.0, 0.0, 0.0), |_| FLOOR.to_vec()), first);
        let far = loose.world_holding(Vector::new(5000.0, 0.0, 0.0), |_| FLOOR.to_vec());
        assert_ne!(far, first);
        assert_eq!(loose.worlds.len(), 2);
        for k in 2..=WORLDS {
            loose.world_holding(Vector::new(0.0, 5000.0 * k as f32, 0.0), |_| Vec::new());
        }
        assert_eq!(loose.worlds.len(), WORLDS);
        assert!(loose.worlds.iter().all(|place| place.id != first), "past the cap the oldest world goes");
    }

    #[test]
    fn a_world_retired_in_flight_takes_its_flying_pieces_and_leaves_the_lying_ones() {
        let mut entities = bevy::ecs::world::World::new();
        let mut loose = Loose::default();
        for k in 0..WORLDS {
            let mut group = lying_group(PieceKind::Flat, Vector::new(5000.0 * k as f32, 0.0, 200.0), 30.0);
            group.pieces[0].entity = entities.spawn_empty().id();
            let id = loose.world_holding(group.pieces[0].place, |_| FLOOR.to_vec());
            let place = loose.worlds.iter_mut().find(|place| place.id == id).unwrap();
            group.pieces[0].body = Some((id, add_piece(&mut place.world, PieceKind::Flat, &group.pieces[0], Vector::ZERO, Vector::ZERO)));
            loose.lying.push(group);
        }
        let lying = Piece { entity: entities.spawn_empty().id(), body: None, ..loose.lying[0].pieces[0] };
        loose.lying[0].pieces.push(lying);
        let flier = loose.lying[0].pieces[0].entity;
        loose.world_holding(Vector::new(0.0, 90_000.0, 0.0), |_| Vec::new());
        assert_eq!(loose.worlds.len(), WORLDS);
        assert_eq!(loose.gone, vec![flier], "the oldest world's flying shard goes rather than hang");
        assert_eq!(loose.lying[0].pieces.len(), 1);
        assert_eq!(loose.lying[0].pieces[0].entity, lying.entity, "a lying piece of the same group stays");
        assert!(loose.lying[1..].iter().all(|group| group.pieces[0].body.is_some()), "the other worlds keep their pieces");
    }

    fn thrown_by(parent: Entity, kind: PieceKind, pieces: usize, age: f32) -> Group {
        let mut group = lying_group(kind, Vector::ZERO, 30.0);
        group.pieces = vec![group.pieces[0]; pieces];
        (group.parent, group.age) = (parent, age);
        group
    }

    #[test]
    fn a_full_cap_fades_the_oldest_lying_pieces_and_sparks_never_take_their_place() {
        let mut world = bevy::ecs::world::World::new();
        let [first, second, third, shot] = [(); 4].map(|()| world.spawn_empty().id());
        let mut loose = Loose::default();
        loose.lying.push(thrown_by(first, PieceKind::Flat, 20, 10.0));
        loose.lying.push(thrown_by(second, PieceKind::Flat, 20, 5.0));
        assert!(loose.make_room(third, PieceKind::Flat, 64), "a third break is thrown under the cap");
        assert!(loose.lying.iter().all(|held| !held.ending));

        assert!(loose.make_room(third, PieceKind::Flat, 40), "at the cap the oldest makes room");
        assert!(loose.lying[0].ending && !loose.lying[1].ending);
        assert_eq!((loose.lying.len(), loose.lying[0].seconds), (2, 10.0 + FADE), "it fades, it is not taken away at once");
        assert!(loose.make_room(third, PieceKind::Flat, 40), "and no longer counts");
        assert!(!loose.lying[1].ending);

        for _ in 0..40 {
            assert!(loose.make_room(shot, PieceKind::Spark, 40), "sparks have a cap of their own");
        }
        assert!(!loose.lying[1].ending, "a burst of sparks fades no lying piece");
        loose.sparks.push(thrown_by(shot, PieceKind::Spark, 40, 0.5));
        assert!(loose.make_room(third, PieceKind::Spark, 40));
        assert!(loose.sparks.is_empty() && loose.lying.len() == 2, "the oldest sparks go at once");
        assert_eq!(loose.gone.len(), 40);
    }

    #[test]
    fn one_break_takes_a_share_of_the_cap_only() {
        let mut world = bevy::ecs::world::World::new();
        let [window, other] = [(); 2].map(|()| world.spawn_empty().id());
        let mut loose = Loose::default();
        loose.lying.push(thrown_by(window, PieceKind::Flat, 64 / BREAK_SHARE, 0.1));
        assert!(!loose.make_room(window, PieceKind::Flat, 64), "the same break throws no more");
        assert!(loose.make_room(other, PieceKind::Flat, 64), "another break does");
        assert!(loose.make_room(window, PieceKind::Spark, 64), "a spark is no part of a break's share");
    }

    #[test]
    fn pieces_are_capped() {
        assert_eq!(piece_cap(0), 0);
        assert_eq!(piece_cap(1), 64);
        assert_eq!(piece_cap(10), 640);
        assert_eq!(piece_cap(20), 640);
        assert_eq!(groups_to_drop(&[10, 10, 10], None, 32), Some(0));
        assert_eq!(groups_to_drop(&[10, 10, 12], None, 32), Some(1));
        assert_eq!(groups_to_drop(&[20, 10, 12], Some(2), 32), Some(1), "the oldest go first");
        assert_eq!(groups_to_drop(&[32], Some(0), 32), None, "a group never drops itself");
        assert_eq!(groups_to_drop(&[], None, 0), None);
    }
}
