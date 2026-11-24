//! Liquid in the air: feeders pour ribbons, wounds spurt and drips hang, all stepped on the CPU against the level and
//! drawn as two meshes, one for the ribbons and one for the drops, lit by the light the particles take.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Asset, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{NoFrustumCulling, Visibility};
use bevy::ecs::message::MessageWriter;
use bevy::ecs::prelude::{Commands, Component, Query, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::resource_exists;
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::math::Vec3;
use bevy::mesh::{Indices, Mesh, Mesh3d, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin, MeshMaterial3d};
use bevy::reflect::TypePath;
use bevy::render::RenderApp;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};
use bevy::time::Time;
use bevy::transform::components::Transform;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_LIQUIDS, Settings};
use p2_math::Vector;
use p2_vm::FluidType;
use rustc_hash::FxHashMap;

use super::LiquidLanding;
use super::drops::{CROWN_SECONDS, DETACH_RADIUS, Drops, Feed, Phase, drop_cap, due, heartbeat, sphere_volume};
use super::stream_ribbon::{reach, ring_vertices, stream_arc};
use crate::bsp_light::FrameLights;
use crate::frame_facts::FrameFacts;
use crate::glow_lights::GlowLights;
use crate::level_collision::LevelCollision;
use crate::particle_draw::light::{ActorLight, ParticleLights};
use crate::particle_draw::{ParticleDrawSystems, draw_particles};
use crate::particle_frame::{DrawnActor, DrawnParticles, Liquid, LiquidRole, Pour};
use crate::screen_pass::modern_level;
use crate::shaders::{add_libraries, add_shaders, shader_file};
use crate::sun_levels::SunLevelAtlas;

const RIBBON_SHADER: Handle<Shader> = uuid_handle!("b2d84f61-7a3e-4c95-8e10-5f6c29a1d7e3");
const DROPS_SHADER: Handle<Shader> = uuid_handle!("4c7e1a92-d35b-4f08-a6e2-91b0c84f3d56");
/// `sun_levels.wesl` reads the material's bindings 5 and 6.
const PARTICLE_LEVELS: &str = "P2_PARTICLE_LEVELS";
/// A wound's drops for each particle its emitter spawns, and their speed between beats against the systole's.
const SPURT_SHARE: f32 = 0.25;
const SPURT_SLOW: f32 = 0.5;
const SPURT_RADIUS: [f32; 2] = [0.35, 0.8];
/// Drops a second a blood stream throws where it breaks, and their size.
const BREAK_DROPS: f32 = 40.0;
const BREAK_RADIUS: [f32; 2] = [0.25, 0.5];
const BREAK_SCATTER: [f32; 2] = [0.85, 1.15];
/// A stream with no strip of its own is drawn along its aim for this long, in this many points.
const AIMED_SECONDS: f32 = 0.6;
const AIMED_POINTS: usize = 12;

pub struct AirLiquidsPlugin;

impl Plugin for AirLiquidsPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(
            app,
            [
                (RIBBON_SHADER, shader_file("liquid_ribbon.wesl")),
                (DROPS_SHADER, shader_file("liquid_drops.wesl")),
            ],
        );
        app.add_plugins(MaterialPlugin::<LiquidAirMaterial>::default());
        app.init_resource::<AirLiquids>().add_systems(
            PostUpdate,
            (follow_air_lights.run_if(resource_exists::<FrameLights>).run_if(resource_exists::<GlowLights>), fly_liquids)
                .chain()
                .after(draw_particles)
                .in_set(ParticleDrawSystems),
        );
    }
}

/// Which of the two draws a material and its mesh are.
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AirPass {
    Ribbon,
    Drops,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
#[bind_group_data(AirPass)]
pub struct LiquidAirMaterial {
    #[texture(5)]
    #[sampler(6)]
    sun_levels: Option<Handle<Image>>,
    pass: AirPass,
}

impl From<&LiquidAirMaterial> for AirPass {
    fn from(material: &LiquidAirMaterial) -> Self {
        material.pass
    }
}

impl Material for LiquidAirMaterial {
    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(DROPS_SHADER)
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(DROPS_SHADER)
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
            Mesh::ATTRIBUTE_TANGENT.at_shader_location(3),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(4),
        ])?];
        descriptor.vertex.shader_defs.push(PARTICLE_LEVELS.into());
        if key.bind_group_data == AirPass::Ribbon {
            descriptor.vertex.shader = RIBBON_SHADER;
        }
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push(PARTICLE_LEVELS.into());
            if key.bind_group_data == AirPass::Ribbon {
                fragment.shader = RIBBON_SHADER;
            }
        }
        Ok(())
    }
}

/// What a vertex's `look` word says it draws; matches the `SHAPE_*` constants of `liquid_look.wesl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Ribbon,
    Fall,
    Crown,
    Hang,
}

impl Shape {
    /// The radius, the `FluidType` byte, the shape and one word more: a ribbon's burning, a crown's age or a drip's growth.
    pub fn look(self, radius: f32, kind: FluidType, extra: f32) -> [f32; 4] {
        [radius, f32::from(kind as u8), f32::from(self as u8), extra]
    }
}

/// Vertices as `liquid_look.wesl`'s `AirIn` takes them: the middle, a way (a ribbon's direction, a drop's motion over
/// the frame or a crown's normal), the corner, the light and the look.
#[derive(Default)]
pub struct AirMesh {
    centers: Vec<[f32; 3]>,
    ways: Vec<[f32; 3]>,
    corners: Vec<[f32; 2]>,
    lights: Vec<[f32; 4]>,
    looks: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl AirMesh {
    pub fn vertex_count(&self) -> u32 {
        self.centers.len() as u32
    }

    pub fn vertex(&mut self, center: Vec3, way: Vec3, corner: [f32; 2], light: Vec3, look: [f32; 4]) {
        self.centers.push(center.to_array());
        self.ways.push(way.to_array());
        self.corners.push(corner);
        self.lights.push(light.extend(1.0).to_array());
        self.looks.push(look);
    }

    fn quad(&mut self, center: Vec3, way: Vec3, light: Vec3, look: [f32; 4]) {
        let first = self.vertex_count();
        for corner in [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]] {
            self.vertex(center, way, corner, light, look);
        }
        self.indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }

    fn into_mesh(self) -> Option<Mesh> {
        (!self.indices.is_empty()).then(|| {
            Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.centers)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.ways)
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.corners)
                .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, self.lights)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.looks)
                .with_inserted_indices(Indices::U32(self.indices))
        })
    }
}

struct Source {
    light: ActorLight,
    /// This frame's light, display brightness; white while the particles are unlit.
    shown: Vec3,
    spurts_owed: f32,
    breaks_owed: f32,
}

impl Default for Source {
    fn default() -> Self {
        Self { light: ActorLight::default(), shown: Vec3::ONE, spurts_owed: 0.0, breaks_owed: 0.0 }
    }
}

#[derive(Resource, Default)]
struct AirLiquids {
    drops: Drops,
    sources: FxHashMap<u64, Source>,
    level: u32,
    /// Seconds of play, the wounds' heartbeat clock.
    clock: f32,
}

fn pour_of(actor: &DrawnActor) -> Option<(Liquid, Pour)> {
    let liquid = actor.liquid?;
    let pour = liquid.pour.filter(|_| matches!(liquid.role, LiquidRole::Stream | LiquidRole::Drip))?;
    Some((liquid, pour))
}

fn follow_air_lights(settings: Option<Res<Settings>>, drawn: Option<Res<DrawnParticles>>, lights: ParticleLights, mut air: ResMut<AirLiquids>) {
    let Some(drawn) = drawn else { return };
    let glow = lights.wanted(settings.as_deref());
    for actor in drawn.actors.iter().filter(|actor| pour_of(actor).is_some()) {
        let source = air.sources.entry(actor.parent.to_bits()).or_default();
        lights.follow(&mut source.light, actor, glow);
        source.shown = if glow.is_some() { source.light.header()[0] } else { Vec3::ONE };
    }
}

#[derive(SystemParam)]
struct AirInputs<'w> {
    settings: Option<Res<'w, Settings>>,
    time: Res<'w, Time>,
    facts: Res<'w, FrameFacts>,
    drawn: Option<Res<'w, DrawnParticles>>,
    collision: Option<Res<'w, LevelCollision>>,
}

#[derive(SystemParam)]
struct AirAssets<'w, 's> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<LiquidAirMaterial>>,
    atlas: Option<Res<'w, SunLevelAtlas>>,
    drawn: Query<'w, 's, (&'static mut Visibility, &'static Mesh3d, &'static MeshMaterial3d<LiquidAirMaterial>, &'static AirPass)>,
}

/// One frame of a feeder's pour: where it starts and its aim, in Bevy coordinates from the world's origin.
struct Pouring<'a> {
    actor: &'a DrawnActor,
    liquid: Liquid,
    pour: Pour,
    origin: Vec3,
    seconds: f32,
    cap: usize,
}

fn fly_liquids(mut commands: Commands, inputs: AirInputs, mut air: ResMut<AirLiquids>, mut assets: AirAssets, mut landings: MessageWriter<LiquidLanding>) {
    let level = modern_level(inputs.settings.as_deref(), MODERN_LIQUIDS);
    let air = &mut *air;
    let Some(drawn) = inputs.drawn.as_deref().filter(|_| level > 0 && inputs.facts.in_play) else {
        air.drops.list.clear();
        show_mesh(&mut assets, &mut commands, AirPass::Ribbon, AirMesh::default());
        show_mesh(&mut assets, &mut commands, AirPass::Drops, AirMesh::default());
        return;
    };
    if air.level != drawn.level {
        (air.level, air.drops) = (drawn.level, Drops::default());
        air.sources.clear();
    }
    let seconds = if inputs.facts.paused { 0.0 } else { inputs.time.delta_secs().min(0.1) };
    air.clock += seconds;
    let (cap, origin) = (drop_cap(level), inputs.facts.origin);
    let level_collision = inputs.collision.as_deref().and_then(|collision| collision.0.as_deref());
    let hit = |from: Vec3, to: Vec3| {
        let unreal = |at: Vec3| Vector::from(to_bevy(at - origin));
        let hit = level_collision?.first_hit(unreal(from), unreal(to))?;
        Some((Vec3::from(to_bevy(hit.location)) + origin, Vec3::from(to_bevy(hit.normal)).normalize_or_zero()))
    };
    let mut ribbons = AirMesh::default();
    for actor in drawn.actors.iter().filter(|actor| actor.drawn) {
        let Some((liquid, pour)) = pour_of(actor).filter(|(_, pour)| pour.flowing) else { continue };
        pour_feeder(air, &Pouring { actor, liquid, pour, origin, seconds, cap }, &hit, &mut ribbons);
    }
    let mut landed = Vec::new();
    air.drops.step(seconds, cap, hit, &mut landed);
    for landing in landed {
        landings.write(landing);
    }
    let mut beads = AirMesh::default();
    for drop in &air.drops.list {
        let light = air.sources.get(&drop.source).map_or(Vec3::ONE, |source| source.shown);
        let (shape, way, extra) = match drop.phase {
            Phase::Fall => (Shape::Fall, drop.velocity * seconds, 0.0),
            Phase::Crown => (Shape::Crown, drop.normal, drop.age / CROWN_SECONDS),
            Phase::Hang => (Shape::Hang, Vec3::Y, drop.radius / DETACH_RADIUS),
        };
        beads.quad(drop.at, way, light, shape.look(drop.radius, drop.kind, extra));
    }
    show_mesh(&mut assets, &mut commands, AirPass::Ribbon, ribbons);
    show_mesh(&mut assets, &mut commands, AirPass::Drops, beads);
}

fn pour_feeder(air: &mut AirLiquids, pouring: &Pouring, hit: &impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>, ribbons: &mut AirMesh) {
    let Pouring { actor, liquid, pour, origin, seconds, cap } = *pouring;
    let id = actor.parent.to_bits();
    let source = air.sources.entry(id).or_default();
    let from = Vec3::from(to_bevy(pour.from)) + origin;
    let velocity = pour.velocity.map(|end| Vec3::from(to_bevy(end)));
    if liquid.role == LiquidRole::Drip {
        air.drops.feed_drip(id, from, liquid.kind, pour.rate.max(0.0) * seconds * sphere_volume(DETACH_RADIUS), cap);
        return;
    }
    // Each wound beats on its own phase.
    let beat = heartbeat(air.clock + (id % 97) as f32 * 0.37);
    let strip = actor.emitters.iter().find(|emitter| !emitter.strip.is_empty());
    let points: Vec<Vec3> = match strip {
        Some(emitter) => emitter.strip.iter().map(|point| Vec3::from(to_bevy(point.location)) + origin).collect(),
        None => aimed(from, (velocity[0] + velocity[1]) * 0.5, Vec3::from(to_bevy(pour.acceleration)), hit),
    };
    let rings = stream_arc(from, &points, reach(liquid.kind, beat));
    ring_vertices(&rings, liquid.kind, f32::from(u8::from(liquid.burning)), source.shown, ribbons);
    if liquid.kind != FluidType::Blood {
        return;
    }
    let wound = Feed { source: id, kind: liquid.kind, from, velocity, radius: SPURT_RADIUS, pours: true };
    let count = due(&mut source.spurts_owed, pour.rate * SPURT_SHARE * beat, seconds);
    air.drops.throw_from(&wound, SPURT_SLOW + (1.0 - SPURT_SLOW) * beat, count, seconds, cap);
    if let Some(end) = rings.last() {
        let speed = (velocity[0] + velocity[1]).length() * 0.5;
        let broken = Feed { from: end.at, velocity: BREAK_SCATTER.map(|scale| end.way * speed * scale), radius: BREAK_RADIUS, pours: false, ..wound };
        let count = due(&mut source.breaks_owed, BREAK_DROPS, seconds);
        air.drops.throw_from(&broken, 1.0, count, seconds, cap);
    }
}

/// The path of a pour along its aim until it meets the level.
fn aimed(from: Vec3, velocity: Vec3, gravity: Vec3, hit: &impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>) -> Vec<Vec3> {
    let mut points = vec![from];
    for step in 1..=AIMED_POINTS {
        let time = AIMED_SECONDS * step as f32 / AIMED_POINTS as f32;
        let next = from + velocity * time + gravity * (0.5 * time * time);
        let last = points[points.len() - 1];
        if let Some((at, _)) = hit(last, next) {
            points.push(at);
            break;
        }
        points.push(next);
    }
    points
}

fn show_mesh(assets: &mut AirAssets, commands: &mut Commands, pass: AirPass, mesh: AirMesh) {
    let levels = assets.atlas.as_deref().and_then(|atlas| atlas.image.clone());
    let built = mesh.into_mesh();
    let Some((mut visibility, shown, material, _)) = assets.drawn.iter_mut().find(|(.., drawn)| **drawn == pass) else {
        if let Some(built) = built {
            let material = assets.materials.add(LiquidAirMaterial { sun_levels: levels, pass });
            commands.spawn((Mesh3d(assets.meshes.add(built)), MeshMaterial3d(material), Transform::IDENTITY, NoFrustumCulling, pass));
        }
        return;
    };
    let Some(built) = built else {
        *visibility = Visibility::Hidden;
        return;
    };
    *visibility = Visibility::Inherited;
    if let Err(error) = assets.meshes.insert(shown.0.id(), built) {
        bevy::log::error!("liquid mesh: {error}");
    }
    if let Some(mut material) = assets.materials.get_mut(material.0.id())
        && material.sun_levels != levels
    {
        material.sun_levels = levels;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    const LOOK: &str = include_str!("../liquid_look.wesl");

    #[test]
    fn the_shader_knows_the_shapes() {
        for (shape, name) in [(Shape::Ribbon, "SHAPE_RIBBON"), (Shape::Fall, "SHAPE_FALL"), (Shape::Crown, "SHAPE_CROWN"), (Shape::Hang, "SHAPE_HANG")] {
            assert_eq!(shader_const(LOOK, name), f64::from(shape as u8), "{name}");
        }
        assert_eq!(Shape::Crown.look(0.5, FluidType::Blood, 0.25), [0.5, 3.0, 2.0, 0.25]);
    }

    #[test]
    fn a_pour_with_no_strip_follows_its_aim_and_stops_at_the_level() {
        let floor = |from: Vec3, to: Vec3| (from.y > 0.0 && to.y <= 0.0).then(|| (from.lerp(to, from.y / (from.y - to.y)), Vec3::Y));
        let points = aimed(Vec3::new(0.0, 10.0, 0.0), Vec3::new(100.0, 0.0, 0.0), Vec3::NEG_Y * 950.0, &floor);
        assert!(points.len() > 2 && points.len() <= AIMED_POINTS + 1);
        let last = points[points.len() - 1];
        assert!(last.y.abs() < 1e-4 && last.x > 0.0, "{last}");
        let mut quads = AirMesh::default();
        quads.quad(Vec3::ZERO, Vec3::Y, Vec3::ONE, Shape::Fall.look(1.0, FluidType::Urine, 0.0));
        assert_eq!((quads.vertex_count(), quads.indices.len()), (4, 6));
        assert!(quads.into_mesh().is_some() && AirMesh::default().into_mesh().is_none());
    }
}
