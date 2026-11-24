use rustc_hash::FxHashMap;
use std::sync::Arc;

use bevy::app::{App, Plugin};
use bevy::camera::visibility::RenderLayers;
use bevy::asset::{Asset, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::ecs::prelude::{Commands, Component, Entity, Has, Local, Query, Res, ResMut, With};
use bevy::ecs::system::SystemParam;
use bevy::time::Time;
use bevy::math::{Vec3, Vec4};
use bevy::image::Image;
use bevy::mesh::{Mesh, Mesh3d, MeshVertexBufferLayoutRef};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, MeshMaterial3d, StandardMaterial};
use bevy::reflect::Reflect;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};
use p2_math::{Rotator, Vector};
use p2_assets::{Model, SkeletalMesh, StaticMesh, StaticMeshInstance, uv_matrix};
use p2_render_cpu::{HardwareKind, HardwareLight, LE_SUNLIGHT, MeshLight, MeshPlacement, Walls, actor_ambient, hardware_ambient, leaf_lights, leaves_ambient, relevant_lights, static_mesh_colors};
use p2_bevy_assets::{lighting_colors, set_vertex_colors, to_bevy};
use p2_vm::ObjectId;

use crate::map_lights::MapLights;
use crate::mesh_light::MeshLitMaterial;
use crate::view::set_specular_uv;
use crate::components::{CanvasDrawn, OVERLAY_LAYERS};
use p2_host::display::Settings;
use crate::view::ShownScene;
use crate::scene::SectionView;
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::shader_layers::{Layers, ShaderLayers};
use crate::shaders::add_material;

use p2_render_cpu::MAX_HARDWARE_LIGHTS;

mod view_model;
pub use view_model::view_model_lighting;

pub const DEFAULT_MAX_LIGHTS: u8 = 4;

pub struct LightEnvironment {
    pub lights: FxHashMap<i32, MeshLight>,
    pub zone_ambients: Vec<[f32; 3]>,
    pub level_brightness: f32,
    pub sun: Option<(MeshLight, HardwareLight)>,
    /// The still suns that shine together with `sun`: every one of the map, or its zone's after
    /// [`Self::suns_of_one_zone`].
    pub suns: Vec<MeshLight>,
    /// Each zone that holds still suns of its own, after [`Self::suns_of_one_zone`].
    pub zone_suns: Vec<ZoneSuns>,
    pub terrain: Vec<crate::terrain_light::TerrainSector>,
}

pub struct ZoneSuns {
    zone: u8,
    sun: Option<(MeshLight, HardwareLight)>,
    suns: Vec<MeshLight>,
}

/// The suns that shine together: the one that casts the shadow and the still ones around it.
#[derive(Clone, Copy)]
pub(crate) struct SunSet<'a> {
    pub(crate) sun: Option<&'a (MeshLight, HardwareLight)>,
    pub(crate) suns: &'a [MeshLight],
    pub(crate) level_brightness: f32,
}

pub(crate) fn brightest_sun<'a>(lights: impl IntoIterator<Item = &'a MeshLight>, level_brightness: f32) -> Option<(MeshLight, HardwareLight)> {
    let strength = |light: &HardwareLight| light.color.iter().sum::<f32>();
    lights
        .into_iter()
        .filter(|light| light.effect == LE_SUNLIGHT)
        .filter_map(|light| Some((*light, HardwareLight::new(light, level_brightness, Vector::default(), 0.0)?)))
        // Equal suns by location: the lights come in hash order, which differs between runs.
        .max_by(|(light_a, a), (light_b, b)| {
            strength(a)
                .total_cmp(&strength(b))
                .then_with(|| light_a.location.x.total_cmp(&light_b.location.x))
                .then_with(|| light_a.location.y.total_cmp(&light_b.location.y))
                .then_with(|| light_a.location.z.total_cmp(&light_b.location.z))
        })
}

#[derive(Clone, Copy)]
pub struct MeshLighting {
    pub placement: MeshPlacement,
    pub zone: u8,
    pub glow: u8,
}

pub fn mesh_sphere(mesh: &StaticMesh) -> (Vector, f32) {
    let (center, radius) = points_sphere(&mesh.positions);
    (center.into(), radius)
}

pub fn points_sphere(points: &[[f32; 3]]) -> ([f32; 3], f32) {
    if points.is_empty() {
        return ([0.0; 3], 0.0);
    }
    let (lo, hi) = points.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| {
        ([0, 1, 2].map(|i| lo[i].min(p[i])), [0, 1, 2].map(|i| hi[i].max(p[i])))
    });
    let center = [0, 1, 2].map(|i| (lo[i] + hi[i]) / 2.0);
    let half = [0, 1, 2].map(|i| (hi[i] - lo[i]) / 2.0);
    (center, (half[0] * half[0] + half[1] * half[1] + half[2] * half[2]).sqrt())
}

/// Retail walks the actor's own render box; the sphere's bounding cube can touch a few more leaves (unverified).
fn sphere_leaves(model: &Model, center: Vector, radius: f32) -> Vec<usize> {
    let reach = Vector::new(radius, radius, radius);
    p2_collision::box_leaves(model, center - reach, center + reach)
}

fn placed_sphere((center, radius): (Vector, f32), placement: &MeshPlacement) -> (Vector, f32) {
    let scale = <[f32; 3]>::from(placement.scale).iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (placement.transform_point(center), radius * scale)
}

impl LightEnvironment {
    pub fn new(lights: FxHashMap<i32, MeshLight>, zone_ambients: Vec<[f32; 3]>, level_brightness: f32) -> Self {
        let mut ordered: Vec<(&i32, &MeshLight)> = lights.iter().collect();
        // Highest key first: of fully equal suns the picker keeps the last, so the lowest key wins.
        ordered.sort_unstable_by_key(|&(&key, _)| std::cmp::Reverse(key));
        let sun = brightest_sun(ordered.into_iter().map(|(_, light)| light), level_brightness);
        let suns = lights.values().filter(|light| light.effect == LE_SUNLIGHT && !light.dynamic).copied().collect();
        Self { lights, zone_ambients, level_brightness, sun, suns, zone_suns: Vec::new(), terrain: Vec::new() }
    }

    /// A map holds a copy of its suns in each outdoor zone, and a copy lights its own zone only (unverified in the
    /// game's code; the maps repeat one set of suns many times over). Counting every copy made such a map many times
    /// too bright, so only the picked sun's zone counts. The other zones' sets are kept for an eye that stands in them;
    /// a set is the suns that light every surface, never the ones for `bSpecialLit` surfaces alone.
    pub fn suns_of_one_zone(mut self, zone_of: impl Fn(Vector) -> u8) -> Self {
        for light in self.suns.iter().filter(|light| !light.special_lit) {
            let zone = zone_of(light.location);
            match self.zone_suns.iter_mut().find(|set| set.zone == zone) {
                Some(set) => set.suns.push(*light),
                None => self.zone_suns.push(ZoneSuns { zone, sun: None, suns: vec![*light] }),
            }
        }
        for set in &mut self.zone_suns {
            set.sun = brightest_sun(&set.suns, self.level_brightness);
        }
        if let Some((sun, _)) = self.sun {
            let zone = zone_of(sun.location);
            self.suns.retain(|light| light.special_lit == sun.special_lit && zone_of(light.location) == zone);
        }
        self
    }

    pub(crate) fn sun_set(&self) -> SunSet<'_> {
        SunSet { sun: self.sun.as_ref(), suns: &self.suns, level_brightness: self.level_brightness }
    }

    pub(crate) fn zone_sun_set(&self, zone: u8) -> Option<SunSet<'_>> {
        let set = self.zone_suns.iter().find(|set| set.zone == zone)?;
        Some(SunSet { sun: set.sun.as_ref(), suns: &set.suns, level_brightness: self.level_brightness })
    }

    pub fn with_terrain(self, terrain: Vec<crate::terrain_light::TerrainSector>) -> Self {
        Self { terrain, ..self }
    }

    pub fn ambient(&self, zone: u8, glow: u8) -> [f32; 3] {
        actor_ambient(self.zone_ambients.get(usize::from(zone)).copied().unwrap_or_default(), glow)
    }

    /// Uses the baked list of lights reaching each of `leaves`, then with `Walls::Checked` a line check from `center` to each light.
    pub fn hardware_lights(&self, model: &Model, leaves: &[usize], center: Vector, radius: f32, max: usize, walls: Walls) -> Vec<HardwareLight> {
        let listed = leaf_lights(model, leaves);
        let candidates = listed.iter().filter_map(|light| self.lights.get(light));
        relevant_lights(model, candidates, center, radius, max, walls)
            .iter()
            .filter_map(|light| HardwareLight::new(light, self.level_brightness, center, radius))
            .collect()
    }

    pub fn mesh_colors(&self, model: &Model, mesh: &StaticMesh, sphere: (Vector, f32), instance: Option<&StaticMeshInstance>, lighting: &MeshLighting) -> Vec<[u8; 4]> {
        let lights: Vec<Option<MeshLight>> =
            instance.map_or_else(Vec::new, |i| i.lights.iter().map(|l| self.lights.get(&l.actor).copied()).collect());
        let (center, radius) = placed_sphere(sphere, &lighting.placement);
        let hardware = self.hardware_lights(model, &sphere_leaves(model, center, radius), center, radius, MAX_HARDWARE_LIGHTS, Walls::Ignored);
        let ambient = self.ambient(lighting.zone, lighting.glow);
        static_mesh_colors(mesh, instance, &lights, &lighting.placement, self.level_brightness, ambient, &hardware)
    }

    pub fn skinned_lighting(&self, model: &Model, center: Vector, radius: f32, glow: u8, max_lights: usize, origin: Vec3) -> SkinnedLighting {
        let leaves = sphere_leaves(model, center, radius);
        let ambient = actor_ambient(leaves_ambient(model, &leaves, &self.zone_ambients), glow);
        let [r, g, b] = hardware_ambient(ambient).map(|c| c / 255.0);
        let lights = self.hardware_lights(model, &leaves, center, radius, max_lights, Walls::Checked);
        let mut out = SkinnedLighting { ambient: Vec3::new(r, g, b), count: lights.len() as u32, lights: [HardwareLightUniform::default(); MAX_HARDWARE_LIGHTS] };
        for (gpu, light) in out.lights.iter_mut().zip(&lights) {
            *gpu = HardwareLightUniform::new(light, origin);
        }
        out
    }
}

/// Lit like D3D vertex lighting: texture × (2 · min(1, ambient + Σ lights))^2.2, then fog.
pub type ActorLitMaterial = ExtendedMaterial<StandardMaterial, HardwareLighting>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
#[data(100, ActorShading, binding_array(107))]
#[bindless(index_table(range(100..107), binding(108)))]
pub struct HardwareLighting {
    pub lighting: SkinnedLighting,
    pub layers: ShaderLayers,
    /// 1 when the overlay camera draws it: no tone pass follows that camera, so under modern the shader applies the curve.
    pub overlay: u32,
    #[texture(101)]
    #[sampler(102)]
    pub detail: Option<Handle<Image>>,
    #[texture(103, dimension = "cube")]
    #[sampler(104)]
    pub specular: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    pub flat_specular: Option<Handle<Image>>,
}

impl HardwareLighting {
    pub fn new(lighting: SkinnedLighting, layers: &Layers) -> Self {
        let [detail, specular, flat_specular] = layers.images();
        Self { lighting, layers: layers.params, overlay: 0, detail, specular, flat_specular }
    }
}

#[derive(ShaderType, Reflect, Debug, Clone, Copy, Default, PartialEq)]
pub struct ActorShading {
    pub lighting: SkinnedLighting,
    pub layers: ShaderLayers,
    pub overlay: u32,
}

impl From<&HardwareLighting> for ActorShading {
    fn from(extension: &HardwareLighting) -> Self {
        Self { lighting: extension.lighting, layers: extension.layers, overlay: extension.overlay }
    }
}

#[derive(ShaderType, Reflect, Debug, Clone, Copy, Default, PartialEq)]
pub struct SkinnedLighting {
    pub ambient: Vec3,
    pub count: u32,
    pub lights: [HardwareLightUniform; MAX_HARDWARE_LIGHTS],
}

#[derive(ShaderType, Reflect, Debug, Clone, Copy, Default, PartialEq)]
pub struct HardwareLightUniform {
    /// xyz: position, w: range.
    pub position: Vec4,
    pub direction: Vec3,
    /// 0 point, 1 spot, 2 directional.
    pub kind: u32,
    pub color: Vec4,
    /// x: constant, y: linear attenuation.
    pub attenuation: Vec4,
}

impl HardwareLightUniform {
    fn new(light: &HardwareLight, origin: Vec3) -> Self {
        let kind = match light.kind {
            HardwareKind::Point => 0,
            HardwareKind::Spot => 1,
            HardwareKind::Directional => 2,
        };
        let position = Vec3::from(to_bevy(light.position)) + origin;
        let [r, g, b] = light.color;
        Self {
            position: position.extend(light.range),
            direction: Vec3::from(to_bevy(light.direction)),
            kind,
            color: Vec4::new(r, g, b, 0.0),
            attenuation: Vec4::new(light.attenuation[0], light.attenuation[1], 0.0, 0.0),
        }
    }
}

const ACTOR_LIGHT_SHADER: Handle<Shader> = uuid_handle!("6d0f2c8e-4b71-4a39-9e15-c2a7f3b80d64");

impl MaterialExtension for HardwareLighting {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::shaders::cast_both_sides(descriptor, key.mesh_key);
        crate::reflections::write_class_in_prepass(descriptor, crate::reflections::LayersHeld::InActorShading);
        Ok(())
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(ACTOR_LIGHT_SHADER)
    }
}

pub struct ActorLightPlugin;

impl Plugin for ActorLightPlugin {
    fn build(&self, app: &mut App) {
        add_material::<ActorLitMaterial>(app, ACTOR_LIGHT_SHADER, include_str!("actor_light.wesl"), "p2_render/actor_light.wesl");
    }
}

#[derive(Clone)]
pub enum Shading {
    Vertices { mesh: Arc<StaticMesh>, sphere: (Vector, f32), scale: Vector, pre_pivot: Vector },
    Tint,
}

#[derive(Component, Clone)]
#[require(ActorFrame, ActorLightCache, ActorLightFade)]
pub struct ActorLit {
    pub actor: ObjectId,
    pub max_lights: usize,
    pub radius: f32,
    pub shading: Shading,
    /// Where its vertex colours were made as the map loaded, when they were.
    baked: Option<ActorPlace>,
    layers: Layers,
}

#[derive(Component, Clone, Copy, Default, PartialEq)]
pub struct ActorFrame {
    pub place: Option<ActorPlace>,
    pub base: Option<ObjectId>,
}

#[derive(Component)]
pub struct ActorLightCache {
    lit: Option<ActorPlace>,
    vertices_lit_at: f32,
    view_model: bool,
}

impl Default for ActorLightCache {
    fn default() -> Self {
        Self { lit: None, vertices_lit_at: f32::MIN, view_model: false }
    }
}

#[derive(Component, Default)]
pub struct ActorLightFade {
    material: Option<Handle<ActorLitMaterial>>,
    target: Option<SkinnedLighting>,
    /// The light on its way to `target`. The material gets it only once it differs by a step a display can show:
    /// every write makes the renderer prepare the material again.
    shown: Option<SkinnedLighting>,
    settling: bool,
}

impl ActorLit {
    pub(crate) fn new(actor: ObjectId, max_lights: usize, radius: f32, shading: Shading) -> Self {
        Self { actor, max_lights, radius, shading, baked: None, layers: Layers::default() }
    }

    pub fn with_layers(self, layers: Layers) -> Self {
        Self { layers, ..self }
    }

    pub fn baked_at(self, location: Vector, rotation: Rotator, glow: u8) -> Self {
        Self { baked: Some(ActorPlace { location, rotation, glow }), ..self }
    }

    pub fn is_lit(&self, cache: &ActorLightCache) -> bool {
        cache.lit.or(self.baked).is_some()
    }

    pub fn skeletal(actor: ObjectId, mesh: &SkeletalMesh, scale: Vector, light: &ActorLighting) -> Option<Self> {
        if light.unlit {
            return None;
        }
        let largest = (0..3).fold(0.0f32, |m, i| m.max((scale[i] * mesh.scale[i]).abs()));
        let radius = points_sphere(&mesh.positions).1 * largest;
        Some(Self::new(actor, usize::from(light.max_lights), radius, Shading::Tint))
    }

    pub fn static_mesh(actor: ObjectId, mesh: Arc<StaticMesh>, scale: Vector, pre_pivot: Vector) -> Self {
        let sphere = mesh_sphere(&mesh);
        // Not its `MaxLights`: retail sets every listed light and the driver takes the first 8.
        Self::new(actor, MAX_HARDWARE_LIGHTS, 0.0, Shading::Vertices { mesh, sphere, scale, pre_pivot })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ActorLighting {
    pub unlit: bool,
    pub glow: u8,
    pub max_lights: u8,
}

fn moved(old: (Vector, Rotator), new: (Vector, Rotator), rotation_counts: bool) -> bool {
    let d = [0, 1, 2].map(|i| old.0[i] - new.0[i]);
    let turned = (0..3).any(|i| (old.1[i] - new.1[i]).rem_euclid(65536).min((new.1[i] - old.1[i]).rem_euclid(65536)) > 256);
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] > 16.0 || (rotation_counts && turned)
}

const VERTEX_UPDATES_PER_FRAME: usize = 64;
/// In seconds. A moving actor's vertex light is made again at most this often: each time sends its whole mesh to the
/// graphics card, and frames come faster than the light on a surface visibly changes.
const VERTEX_RELIGHT_PERIOD: f32 = 1.0 / 60.0;

/// Retail snaps an actor's light at a zone crossing; ours fades in both looks.
const LIGHT_SETTLE_SECONDS: f32 = 0.3;

pub(crate) fn settle_step(dt: f32, seconds: f32) -> f32 {
    1.0 - (-dt / seconds).exp()
}

const SAME_LIGHT_DISTANCE: f32 = 8.0;

const LIGHT_EPSILON: f32 = 1.0 / 255.0;

fn same_light(a: &HardwareLightUniform, b: &HardwareLightUniform) -> bool {
    a.kind == b.kind && a.position.truncate().distance(b.position.truncate()) < SAME_LIGHT_DISTANCE
}

pub fn blend_lighting(shown: &SkinnedLighting, target: &SkinnedLighting, t: f32) -> SkinnedLighting {
    let count = |lighting: &SkinnedLighting| (lighting.count as usize).min(MAX_HARDWARE_LIGHTS);
    let (old, new) = (&shown.lights[..count(shown)], &target.lights[..count(target)]);
    let mut matched = [false; MAX_HARDWARE_LIGHTS];
    let mut lights = [HardwareLightUniform::default(); MAX_HARDWARE_LIGHTS];
    let mut used = 0;
    for light in new {
        let from = match old.iter().enumerate().position(|(i, o)| !matched[i] && same_light(o, light)) {
            Some(i) => {
                matched[i] = true;
                old[i]
            }
            None => HardwareLightUniform { color: Vec4::ZERO, ..*light },
        };
        lights[used] = HardwareLightUniform {
            position: from.position.lerp(light.position, t),
            direction: from.direction.lerp(light.direction, t).normalize_or(light.direction),
            kind: light.kind,
            color: from.color.lerp(light.color, t),
            attenuation: from.attenuation.lerp(light.attenuation, t),
        };
        used += 1;
    }
    for (i, light) in old.iter().enumerate() {
        let color = light.color * (1.0 - t);
        if !matched[i] && used < MAX_HARDWARE_LIGHTS && color.abs().max_element() > LIGHT_EPSILON {
            lights[used] = HardwareLightUniform { color, ..*light };
            used += 1;
        }
    }
    let out = SkinnedLighting { ambient: shown.ambient.lerp(target.ambient, t), count: used as u32, lights };
    if looks_the_same(&out, target) { *target } else { out }
}

fn looks_the_same(a: &SkinnedLighting, b: &SkinnedLighting) -> bool {
    let close = |a: Vec4, b: Vec4| (a - b).abs().max_element() <= LIGHT_EPSILON;
    a.count == b.count
        && close(a.ambient.extend(0.0), b.ambient.extend(0.0))
        && a.lights.iter().zip(&b.lights).all(|(a, b)| {
            a.kind == b.kind && close(a.position, b.position) && close(a.direction.extend(0.0), b.direction.extend(0.0)) && close(a.color, b.color) && close(a.attenuation, b.attenuation)
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActorPlace {
    pub location: Vector,
    pub rotation: Rotator,
    pub glow: u8,
}

#[derive(SystemParam)]
pub(crate) struct ActorLightFrame<'w> {
    shown: Res<'w, ShownScene>,
    collision: Res<'w, LevelCollision>,
    time: Res<'w, Time>,
    settings: Option<Res<'w, Settings>>,
    map_lights: Option<Res<'w, MapLights>>,
}

#[derive(SystemParam)]
pub(crate) struct ActorLightAssets<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: Res<'w, Assets<StandardMaterial>>,
    mesh_lit: Res<'w, Assets<MeshLitMaterial>>,
    lit_materials: ResMut<'w, Assets<ActorLitMaterial>>,
}

struct RelightWorld<'a> {
    environment: &'a LightEnvironment,
    level: &'a StaticCollision,
    model: &'a Model,
    origin: Vec3,
    /// None under the retail look.
    map_lights: Option<&'a MapLights>,
    view_models: &'a mut Vec<(Vector, SkinnedLighting)>,
}

struct StartingMaterial<'a> {
    standard: Option<&'a MeshMaterial3d<StandardMaterial>>,
    vertex_lit: Option<&'a MeshMaterial3d<MeshLitMaterial>>,
}

pub(crate) fn light_actors(
    mut commands: Commands,
    frames: ActorLightFrame,
    mut assets: ActorLightAssets,
    mut models: Query<(
        Entity,
        &mut ActorLit,
        &mut ActorLightCache,
        &mut ActorLightFade,
        &ActorFrame,
        &Mesh3d,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Option<&MeshMaterial3d<MeshLitMaterial>>,
        Has<CanvasDrawn>,
        Option<&RenderLayers>,
    )>,
    canvases: Query<(), With<CanvasDrawn>>,
    mut buffers: Local<LightActorBuffers>,
) {
    let (Some(environment), Some(level)) = (frames.shown.lighting.as_ref(), frames.collision.0.as_deref()) else { return };
    let modern = frames.settings.as_deref().is_some_and(Settings::modern_graphics);
    let fresh;
    // On the frame a new map shows, this runs before the resource follows it.
    let map_lights = match frames.map_lights.as_deref().filter(|map| map.is_for(environment)) {
        _ if !modern => None,
        Some(map) => Some(map),
        None => {
            fresh = MapLights::new(environment);
            Some(&fresh)
        }
    };
    let step = settle_step(frames.time.delta_secs(), LIGHT_SETTLE_SECONDS);
    let now = frames.time.elapsed_secs();
    let LightActorBuffers { colors, hud_bases, view_models } = &mut *buffers;
    colors.clear();
    hud_bases.clear();
    view_models.clear();
    let mut world = RelightWorld { environment, level, model: &level.model, origin: frames.shown.origin, map_lights, view_models };
    if !canvases.is_empty() {
        hud_bases.extend(
            models
                .iter()
                .filter(|(_, lit, _, _, _, _, _, _, canvas, _)| *canvas && matches!(lit.shading, Shading::Tint))
                .map(|(_, lit, _, _, own, ..)| (lit.actor, (own.place, (lit.radius, lit.max_lights)))),
        );
    }
    let overlay_layers = RenderLayers::from_layers(&OVERLAY_LAYERS);
    for (entity, mut lit, mut cache, mut fade, own, mesh, standard, vertex_lit, canvas, layers) in &mut models {
        let overlay = layers.is_some_and(|layers| layers.intersects(&overlay_layers));
        let base = (!canvas && overlay)
            .then_some(own.base)
            .flatten()
            .and_then(|base| hud_bases.get(&base).copied());
        let Some(frame) = base.map_or(own.place, |(place, _)| place) else { continue };
        if let Some((_, sphere)) = base {
            attach_to_hud_base(&mut lit, &mut cache, mesh, sphere, &mut assets.meshes);
        }
        let vertices = matches!(lit.shading, Shading::Vertices { .. });
        let last = cache.lit.or(lit.baked);
        // Modern reads neither hardware lights nor vertex colours; the stale cache makes retail relight a moved actor.
        // One that just left the overlay layers is lit once more, which clears its material's overlay mark.
        if modern && !overlay && !cache.view_model && last.is_some() && (vertices || fade.material.is_some()) {
            continue;
        }
        let view_model = modern && overlay;
        let unmoved = cache.view_model == view_model
            && last.is_some_and(|at| !moved((at.location, at.rotation), (frame.location, frame.rotation), vertices) && at.glow == frame.glow);
        let still = unmoved && !colors.contains_key(&lit.actor);
        if still && !fade.settling {
            continue;
        }
        if vertices {
            if last.is_some() && !colors.contains_key(&lit.actor) && now - cache.vertices_lit_at < VERTEX_RELIGHT_PERIOD {
                continue;
            }
            cache.vertices_lit_at = now;
            if !relight_vertices(&lit, frame, &mesh.0, &world, colors, &mut assets.meshes) {
                continue;
            }
        } else if let Some(handle) = relight_tint(&lit, &mut fade, still, overlay, frame, step, &mut world, &mut assets, StartingMaterial { standard, vertex_lit }) {
            commands
                .entity(entity)
                .remove::<(MeshMaterial3d<StandardMaterial>, MeshMaterial3d<MeshLitMaterial>)>()
                .insert(MeshMaterial3d(handle));
        }
        cache.lit = Some(frame);
        cache.view_model = view_model;
    }
}

#[derive(Default)]
pub(crate) struct LightActorBuffers {
    colors: FxHashMap<ObjectId, Vec<[f32; 4]>>,
    hud_bases: FxHashMap<ObjectId, (Option<ActorPlace>, (f32, usize))>,
    view_models: Vec<(Vector, SkinnedLighting)>,
}

fn attach_to_hud_base(lit: &mut ActorLit, cache: &mut ActorLightCache, mesh: &Mesh3d, (radius, max_lights): (f32, usize), meshes: &mut Assets<Mesh>) {
    if matches!(lit.shading, Shading::Vertices { .. }) {
        if let Some(mut source) = meshes.get_mut(&mesh.0) {
            let count = source.count_vertices();
            source.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; count]);
        }
        lit.shading = Shading::Tint;
        (lit.baked, cache.lit) = (None, None);
    }
    lit.radius = radius;
    lit.max_lights = max_lights;
}

fn relight_vertices(lit: &ActorLit, frame: ActorPlace, target: &Handle<Mesh>, world: &RelightWorld, colors: &mut FxHashMap<ObjectId, Vec<[f32; 4]>>, meshes: &mut Assets<Mesh>) -> bool {
    let Shading::Vertices { mesh: source, sphere, scale, pre_pivot } = &lit.shading else { return true };
    if !colors.contains_key(&lit.actor) {
        if colors.len() >= VERTEX_UPDATES_PER_FRAME {
            return false;
        }
        let placement = MeshPlacement { location: frame.location, rotation: frame.rotation, scale: (*scale), pre_pivot: (*pre_pivot) };
        let lighting = MeshLighting { placement, zone: p2_collision::point_zone(world.model, frame.location), glow: frame.glow };
        colors.insert(lit.actor, lighting_colors(&world.environment.mesh_colors(world.model, source, *sphere, None, &lighting)));
    }
    if let (Some(colors), Some(mut mesh)) = (colors.get(&lit.actor), meshes.get_mut(target)) {
        p2_host::frame_times::mesh_written(p2_host::frame_times::MeshWrite::ActorLight);
        set_vertex_colors(&mut mesh, colors);
    }
    true
}

#[expect(clippy::too_many_arguments, reason = "the parts of one actor's light state and of the frame")]
fn relight_tint(lit: &ActorLit, fade: &mut ActorLightFade, still: bool, overlay: bool, frame: ActorPlace, step: f32, world: &mut RelightWorld, assets: &mut ActorLightAssets, start: StartingMaterial) -> Option<Handle<ActorLitMaterial>> {
    let target = match (fade.target, world.map_lights.filter(|_| overlay)) {
        (Some(target), _) if still => target,
        (_, Some(map)) => match world.view_models.iter().find(|(eye, _)| *eye == frame.location) {
            Some(&(_, lighting)) => lighting,
            None => {
                let lighting = view_model_lighting(world.environment, map, world.level, world.origin, frame.location);
                world.view_models.push((frame.location, lighting));
                lighting
            }
        },
        _ => world.environment.skinned_lighting(world.model, frame.location, lit.radius, frame.glow, lit.max_lights, world.origin),
    };
    fade.target = Some(target);
    let Some(handle) = fade.material.as_ref() else {
        let vertex_lit = start.vertex_lit.and_then(|m| assets.mesh_lit.get(&m.0));
        let base = start.standard.and_then(|s| assets.materials.get(&s.0)).or_else(|| vertex_lit.map(|m| &m.base));
        let base = lit.layers.lit(base.cloned().unwrap_or_default());
        let handle = assets.lit_materials.add(ActorLitMaterial { base, extension: HardwareLighting { overlay: u32::from(overlay), ..HardwareLighting::new(target, &lit.layers) } });
        fade.material = Some(handle.clone());
        return Some(handle);
    };
    let drawn = assets.lit_materials.get(handle).map(|m| (m.extension.lighting, m.extension.overlay));
    let stepped = fade_step(drawn.map(|(lighting, _)| lighting), fade.shown, target, step);
    fade.shown = Some(stepped.shown);
    // A weapon gets its overlay layer after its material is made, and the shader picks its light by this.
    let overlay = u32::from(overlay);
    if (stepped.write || drawn.is_some_and(|(_, drawn)| drawn != overlay)) && let Some(mut material) = assets.lit_materials.get_mut(handle) {
        material.extension.lighting = stepped.shown;
        material.extension.overlay = overlay;
    }
    fade.settling = !stepped.arrived;
    None
}

#[derive(Debug, PartialEq)]
struct LightFade {
    shown: SkinnedLighting,
    arrived: bool,
    write: bool,
}

/// `drawn` is the light the material holds now, `shown` the light on its way to `target`.
fn fade_step(drawn: Option<SkinnedLighting>, shown: Option<SkinnedLighting>, target: SkinnedLighting, step: f32) -> LightFade {
    let lighting = shown.or(drawn).map_or(target, |shown| blend_lighting(&shown, &target, step));
    let arrived = lighting == target;
    let write = drawn.is_some_and(|drawn| if arrived { drawn != lighting } else { !looks_the_same(&drawn, &lighting) });
    LightFade { shown: lighting, arrived, write }
}

pub(crate) fn animate_actor_speculars(
    time: Res<Time>,
    images: Res<Assets<Image>>,
    models: Query<(&ActorLit, &ActorLightFade)>,
    mut lit_materials: ResMut<Assets<ActorLitMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for (lit, fade) in &models {
        let (Some(handle), Some(specular)) = (fade.material.as_ref(), lit.layers.flat_specular.as_ref()) else { continue };
        if lit.layers.specular_uv.is_empty() {
            continue;
        }
        let size = crate::view::image_size(&images, specular);
        let [[a, b, c], [d, e, f]] = uv_matrix(&lit.layers.specular_uv, seconds, size);
        let uv = (Vec4::new(a, b, c, 0.0), Vec4::new(d, e, f, 0.0));
        set_specular_uv(&mut lit_materials, handle, uv, |m| &m.extension.layers, |m| &mut m.extension.layers);
    }
}

pub(crate) fn keep_for_relighting(mesh: &mut Mesh) {
    mesh.asset_usage = RenderAssetUsages::all();
}

pub fn keep_sections_for_relighting(meshes: &mut Assets<Mesh>, sections: &[SectionView]) {
    for section in sections {
        if let Some(mut mesh) = meshes.get_mut(&section.mesh) {
            keep_for_relighting(&mut mesh);
        }
    }
}

pub fn relit_sections(meshes: &mut Assets<Mesh>, sections: &[SectionView]) -> Vec<SectionView> {
    let mut sections = sections.to_vec();
    for section in &mut sections {
        if let Some(copy) = meshes.get(&section.mesh).cloned() {
            section.mesh = meshes.add(copy);
        }
    }
    sections
}

#[cfg(test)]
mod tests;
