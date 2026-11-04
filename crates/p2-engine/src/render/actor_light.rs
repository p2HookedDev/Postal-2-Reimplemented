use std::collections::HashMap;

use rustc_hash::FxHashMap;
use std::sync::Arc;

use bevy::app::{App, Plugin};
use bevy::camera::visibility::RenderLayers;
use bevy::asset::{Asset, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::ecs::prelude::{Commands, Component, Entity, Has, Query, Res, ResMut};
use bevy::ecs::system::SystemParam;
use bevy::time::Time;
use bevy::math::{Vec3, Vec4};
use bevy::image::Image;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MeshMaterial3d, StandardMaterial};
use bevy::reflect::Reflect;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::{Shader, ShaderRef};
use p2_math::{Rotator, Vector};
use p2_assets::{Model, SkeletalMesh, StaticMesh, StaticMeshInstance, uv_matrix};
use p2_render_cpu::{HardwareKind, HardwareLight, MeshLight, MeshPlacement, actor_ambient, hardware_ambient, relevant_lights, static_mesh_colors};
use p2_bevy_assets::{lighting_colors, set_vertex_colors, to_bevy};
use p2_vm::ObjectId;

use crate::render::mesh_light::MeshLitMaterial;
use crate::components::{CanvasDrawn, OVERLAY_LAYERS};
use crate::shown_scene::ShownScene;
use crate::level_collision::LevelCollision;
use crate::render::shader_layers::{Layers, ShaderLayers};
use crate::render::shaders::add_material;

pub use p2_render_cpu::MAX_HARDWARE_LIGHTS;

pub const DEFAULT_MAX_LIGHTS: u8 = 4;

pub struct LightEnvironment {
    pub(crate) lights: FxHashMap<i32, MeshLight>,
    pub(crate) zone_ambients: Vec<[f32; 3]>,
    pub(crate) level_brightness: f32,
    pub(crate) sun: Option<(MeshLight, HardwareLight)>,
    pub(crate) terrain: Vec<crate::render::terrain_light::TerrainSector>,
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

fn placed_sphere((center, radius): (Vector, f32), placement: &MeshPlacement) -> (Vector, f32) {
    let scale = <[f32; 3]>::from(placement.scale).iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (placement.transform_point(center), radius * scale)
}

impl LightEnvironment {
    pub fn new(lights: HashMap<i32, MeshLight>, zone_ambients: Vec<[f32; 3]>, level_brightness: f32) -> Self {
        let sun = crate::render::modern::brightest_sun(lights.values(), level_brightness);
        Self { lights: lights.into_iter().collect(), zone_ambients, level_brightness, sun, terrain: Vec::new() }
    }

    pub fn with_terrain(self, terrain: Vec<crate::render::terrain_light::TerrainSector>) -> Self {
        Self { terrain, ..self }
    }

    pub fn ambient(&self, zone: u8, glow: u8) -> [f32; 3] {
        actor_ambient(self.zone_ambients.get(usize::from(zone)).copied().unwrap_or_default(), glow)
    }

    /// Uses the baked list of lights reaching each leaf, shadows considered. Leaves are sampled at the
    /// centre and half the radius out along each axis, so props set into a wall still find the room's.
    pub fn hardware_lights(&self, model: &Model, center: Vector, radius: f32, max: usize) -> Vec<HardwareLight> {
        let reach = radius / 2.0;
        let along = |axis: usize, side: f32| {
            let mut point = center;
            point[axis] += side * reach;
            point
        };
        let samples = [center, along(0, -1.0), along(0, 1.0), along(1, -1.0), along(1, 1.0), along(2, -1.0), along(2, 1.0)];
        let mut leaves: Vec<usize> = samples.iter().filter_map(|&point| p2_collision::point_leaf(model, point)).collect();
        leaves.sort_unstable();
        leaves.dedup();
        let mut listed: Vec<i32> = Vec::new();
        for leaf in leaves {
            let start = usize::try_from(model.leaves[leaf].permeating).ok();
            let lights = start.and_then(|start| model.lights.get(start..)).unwrap_or_default();
            let earlier = listed.len();
            for &light in lights.iter().take_while(|&&light| light != 0) {
                if !listed[..earlier].contains(&light) {
                    listed.push(light);
                }
            }
        }
        let candidates = listed.iter().filter_map(|light| self.lights.get(light));
        relevant_lights(candidates, center, radius, max)
            .iter()
            .filter_map(|light| HardwareLight::new(light, self.level_brightness, center, radius))
            .collect()
    }

    pub fn mesh_colors(&self, model: &Model, mesh: &StaticMesh, instance: Option<&StaticMeshInstance>, lighting: &MeshLighting) -> Vec<[u8; 4]> {
        let lights: Vec<Option<MeshLight>> =
            instance.map_or_else(Vec::new, |i| i.lights.iter().map(|l| self.lights.get(&l.actor).copied()).collect());
        let (center, radius) = placed_sphere(mesh_sphere(mesh), &lighting.placement);
        let hardware = self.hardware_lights(model, center, radius, MAX_HARDWARE_LIGHTS);
        let ambient = self.ambient(lighting.zone, lighting.glow);
        static_mesh_colors(mesh, instance, &lights, &lighting.placement, self.level_brightness, ambient, &hardware)
    }

    pub fn skinned_lighting(&self, model: &Model, center: Vector, radius: f32, glow: u8, max_lights: usize, origin: Vec3) -> SkinnedLighting {
        let zone = p2_collision::point_zone(model, center);
        let [r, g, b] = hardware_ambient(self.ambient(zone, glow)).map(|c| c / 255.0);
        let lights = self.hardware_lights(model, center, radius, max_lights);
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
        Self { lighting, layers: layers.params, detail, specular, flat_specular }
    }
}

#[derive(ShaderType, Reflect, Debug, Clone, Copy, Default, PartialEq)]
pub struct ActorShading {
    pub lighting: SkinnedLighting,
    pub layers: ShaderLayers,
}

impl From<&HardwareLighting> for ActorShading {
    fn from(extension: &HardwareLighting) -> Self {
        Self { lighting: extension.lighting, layers: extension.layers }
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
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(ACTOR_LIGHT_SHADER)
    }
}

pub struct ActorLightPlugin;

impl Plugin for ActorLightPlugin {
    fn build(&self, app: &mut App) {
        add_material::<ActorLitMaterial>(app, ACTOR_LIGHT_SHADER, include_str!("actor_light.wesl"), "p2_engine/actor_light.wesl");
    }
}

#[derive(Clone)]
pub enum Shading {
    Vertices { mesh: Arc<StaticMesh>, scale: Vector, pre_pivot: Vector },
    Tint,
}

#[derive(Component, Clone)]
pub struct ActorLit {
    pub actor: ObjectId,
    pub glow: u8,
    pub max_lights: usize,
    pub radius: f32,
    pub shading: Shading,
    lit_at: Option<(Vector, Rotator)>,
    material: Option<Handle<ActorLitMaterial>>,
    layers: Layers,
    target: Option<SkinnedLighting>,
    settling: bool,
}

impl ActorLit {
    pub fn new(actor: ObjectId, glow: u8, max_lights: usize, radius: f32, shading: Shading) -> Self {
        Self { actor, glow, max_lights, radius, shading, lit_at: None, material: None, layers: Layers::default(), target: None, settling: false }
    }

    pub fn with_layers(self, layers: Layers) -> Self {
        Self { layers, ..self }
    }

    pub fn lit_at(self, location: Vector, rotation: Rotator) -> Self {
        Self { lit_at: Some((location, rotation)), ..self }
    }

    pub fn is_lit(&self) -> bool {
        self.lit_at.is_some()
    }

    pub fn skeletal(actor: ObjectId, mesh: &SkeletalMesh, scale: Vector, light: &ActorLighting) -> Option<Self> {
        if light.unlit {
            return None;
        }
        let largest = (0..3).fold(0.0f32, |m, i| m.max((scale[i] * mesh.scale[i]).abs()));
        let radius = points_sphere(&mesh.positions).1 * largest;
        Some(Self::new(actor, light.glow, usize::from(light.max_lights), radius, Shading::Tint))
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

/// Retail snaps an actor's light at a zone crossing; ours fades in both looks.
const LIGHT_SETTLE_SECONDS: f32 = 0.3;

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
    let close = |a: Vec4, b: Vec4| (a - b).abs().max_element() <= LIGHT_EPSILON;
    let reached = out.count == target.count
        && close(out.ambient.extend(0.0), target.ambient.extend(0.0))
        && out.lights.iter().zip(&target.lights).all(|(a, b)| {
            a.kind == b.kind && close(a.position, b.position) && close(a.direction.extend(0.0), b.direction.extend(0.0)) && close(a.color, b.color) && close(a.attenuation, b.attenuation)
        });
    if reached { *target } else { out }
}

#[derive(Debug, Clone, Copy)]
pub struct ActorPlace {
    pub location: Vector,
    pub rotation: Rotator,
    pub glow: u8,
}

pub trait ActorFrames: bevy::ecs::prelude::Resource {
    fn origin(&self) -> Option<Vec3>;
    fn actor_place(&self, actor: ObjectId) -> Option<ActorPlace>;
    fn attachment_base(&self, actor: ObjectId) -> Option<ObjectId>;
}

#[derive(SystemParam)]
pub(crate) struct ActorLightFrame<'w, F: ActorFrames> {
    play: Option<Res<'w, F>>,
    shown: Res<'w, ShownScene>,
    collision: Res<'w, LevelCollision>,
    time: Res<'w, Time>,
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
    model: &'a Model,
    origin: Vec3,
}

struct StartingMaterial<'a> {
    standard: Option<&'a MeshMaterial3d<StandardMaterial>>,
    vertex_lit: Option<&'a MeshMaterial3d<MeshLitMaterial>>,
}

pub(crate) fn light_actors<F: ActorFrames>(
    mut commands: Commands,
    frames: ActorLightFrame<F>,
    mut assets: ActorLightAssets,
    mut models: Query<(
        Entity,
        &mut ActorLit,
        &Mesh3d,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Option<&MeshMaterial3d<MeshLitMaterial>>,
        Has<CanvasDrawn>,
        Option<&RenderLayers>,
    )>,
) {
    let (Some(play), Some(environment)) = (frames.play.as_deref(), frames.shown.lighting.as_deref()) else { return };
    let (Some(origin), Some(model)) = (play.origin(), frames.collision.model()) else { return };
    let world = RelightWorld { environment, model, origin };
    let step = 1.0 - (-frames.time.delta_secs() / LIGHT_SETTLE_SECONDS).exp();
    let mut colors: FxHashMap<ObjectId, Vec<[f32; 4]>> = FxHashMap::default();
    let hud_bases: FxHashMap<ObjectId, (f32, usize)> = models
        .iter()
        .filter(|(_, lit, _, _, _, canvas, _)| *canvas && matches!(lit.shading, Shading::Tint))
        .map(|(_, lit, ..)| (lit.actor, (lit.radius, lit.max_lights)))
        .collect();
    let overlay_layers = RenderLayers::from_layers(&OVERLAY_LAYERS);
    for (entity, mut lit, mesh, standard, vertex_lit, canvas, layers) in &mut models {
        let base = (!canvas && layers.is_some_and(|layers| layers.intersects(&overlay_layers)))
            .then(|| play.attachment_base(lit.actor))
            .flatten()
            .and_then(|base| hud_bases.get(&base).map(|&sphere| (base, sphere)));
        let Some(frame) = play.actor_place(base.map_or(lit.actor, |(base, _)| base)) else { continue };
        if let Some((_, sphere)) = base {
            attach_to_hud_base(&mut lit, mesh, sphere, &mut assets.meshes);
        }
        let placed = (frame.location, frame.rotation);
        let vertices = matches!(lit.shading, Shading::Vertices { .. });
        let still = lit.lit_at.is_some_and(|at| !moved(at, placed, vertices)) && lit.glow == frame.glow && !colors.contains_key(&lit.actor);
        if still && !lit.settling {
            continue;
        }
        if vertices {
            if !relight_vertices(&lit, frame, &mesh.0, &world, &mut colors, &mut assets.meshes) {
                continue;
            }
        } else if let Some(handle) = relight_tint(&mut lit, still, frame, step, &world, &mut assets, StartingMaterial { standard, vertex_lit }) {
            commands
                .entity(entity)
                .remove::<(MeshMaterial3d<StandardMaterial>, MeshMaterial3d<MeshLitMaterial>)>()
                .insert(MeshMaterial3d(handle));
        }
        lit.lit_at = Some(placed);
        lit.glow = frame.glow;
    }
}

fn attach_to_hud_base(lit: &mut ActorLit, mesh: &Mesh3d, (radius, max_lights): (f32, usize), meshes: &mut Assets<Mesh>) {
    if matches!(lit.shading, Shading::Vertices { .. }) {
        if let Some(mut source) = meshes.get_mut(&mesh.0) {
            let count = source.count_vertices();
            source.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; count]);
        }
        lit.shading = Shading::Tint;
        lit.lit_at = None;
    }
    lit.radius = radius;
    lit.max_lights = max_lights;
}

fn relight_vertices(lit: &ActorLit, frame: ActorPlace, target: &Handle<Mesh>, world: &RelightWorld, colors: &mut FxHashMap<ObjectId, Vec<[f32; 4]>>, meshes: &mut Assets<Mesh>) -> bool {
    let Shading::Vertices { mesh: source, scale, pre_pivot } = &lit.shading else { return true };
    if !colors.contains_key(&lit.actor) {
        if colors.len() >= VERTEX_UPDATES_PER_FRAME {
            return false;
        }
        let placement = MeshPlacement { location: frame.location, rotation: frame.rotation, scale: (*scale), pre_pivot: (*pre_pivot) };
        let lighting = MeshLighting { placement, zone: p2_collision::point_zone(world.model, frame.location), glow: frame.glow };
        colors.insert(lit.actor, lighting_colors(&world.environment.mesh_colors(world.model, source, None, &lighting)));
    }
    if let (Some(colors), Some(mut mesh)) = (colors.get(&lit.actor), meshes.get_mut(target)) {
        set_vertex_colors(&mut mesh, colors);
    }
    true
}

fn relight_tint(lit: &mut ActorLit, still: bool, frame: ActorPlace, step: f32, world: &RelightWorld, assets: &mut ActorLightAssets, start: StartingMaterial) -> Option<Handle<ActorLitMaterial>> {
    let target = match lit.target {
        Some(target) if still => target,
        _ => world.environment.skinned_lighting(world.model, frame.location, lit.radius, frame.glow, lit.max_lights, world.origin),
    };
    lit.target = Some(target);
    let Some(handle) = lit.material.as_ref() else {
        let vertex_lit = start.vertex_lit.and_then(|m| assets.mesh_lit.get(&m.0));
        let base = start.standard.and_then(|s| assets.materials.get(&s.0)).or_else(|| vertex_lit.map(|m| &m.base));
        let base = lit.layers.lit(base.cloned().unwrap_or_default());
        let handle = assets.lit_materials.add(ActorLitMaterial { base, extension: HardwareLighting::new(target, &lit.layers) });
        lit.material = Some(handle.clone());
        return Some(handle);
    };
    let shown = assets.lit_materials.get(handle).map(|m| m.extension.lighting);
    let lighting = shown.map_or(target, |shown| blend_lighting(&shown, &target, step));
    if shown.is_some_and(|shown| shown != lighting)
        && let Some(mut material) = assets.lit_materials.get_mut(handle)
    {
        material.extension.lighting = lighting;
    }
    lit.settling = lighting != target;
    None
}

pub(crate) fn animate_actor_speculars(
    time: Res<Time>,
    images: Res<Assets<Image>>,
    models: Query<&ActorLit>,
    mut lit_materials: ResMut<Assets<ActorLitMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for lit in &models {
        let (Some(handle), Some(specular)) = (lit.material.as_ref(), lit.layers.flat_specular.as_ref()) else { continue };
        if lit.layers.specular_uv.is_empty() {
            continue;
        }
        let size = images.get(specular).map_or([1.0, 1.0], |image| image.size().as_vec2().to_array());
        let [[a, b, c], [d, e, f]] = uv_matrix(&lit.layers.specular_uv, seconds, size);
        if let Some(mut material) = lit_materials.get_mut(handle) {
            material.extension.layers.specular_u = Vec4::new(a, b, c, 0.0);
            material.extension.layers.specular_v = Vec4::new(d, e, f, 0.0);
        }
    }
}

pub fn keep_for_relighting(mesh: &mut Mesh) {
    mesh.asset_usage = RenderAssetUsages::all();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(x: f32, color: f32) -> HardwareLightUniform {
        HardwareLightUniform {
            position: Vec4::new(x, 0.0, 0.0, 500.0),
            direction: Vec3::new(0.0, -1.0, 0.0),
            kind: 0,
            color: Vec4::new(color, color, color, 0.0),
            attenuation: Vec4::new(1.0, 0.0, 0.0, 0.0),
        }
    }

    fn lighting(ambient: f32, lights: &[HardwareLightUniform]) -> SkinnedLighting {
        let mut out = SkinnedLighting { ambient: Vec3::splat(ambient), count: lights.len() as u32, lights: [HardwareLightUniform::default(); MAX_HARDWARE_LIGHTS] };
        for (slot, light) in out.lights.iter_mut().zip(lights) {
            *slot = *light;
        }
        out
    }

    #[test]
    fn identical_lighting_unchanged() {
        let shown = lighting(0.2, &[light(0.0, 0.5), light(200.0, 0.3)]);
        assert_eq!(blend_lighting(&shown, &shown, 0.1), shown);
    }

    #[test]
    fn blend_converges_to_target() {
        let (mut shown, target) = (lighting(0.1, &[light(0.0, 0.2)]), lighting(0.6, &[light(3.0, 0.9)]));
        let next = blend_lighting(&shown, &target, 0.5);
        assert!((next.ambient.x - 0.35).abs() < 1e-6);
        assert_eq!(next.count, 1);
        for _ in 0..200 {
            shown = blend_lighting(&shown, &target, 0.1);
        }
        assert_eq!(shown, target);
    }

    #[test]
    fn new_light_fades_in() {
        let (shown, target) = (lighting(0.2, &[]), lighting(0.2, &[light(100.0, 1.0)]));
        let next = blend_lighting(&shown, &target, 0.25);
        assert_eq!(next.count, 1);
        assert!((next.lights[0].color.x - 0.25).abs() < 1e-6);
        assert_eq!(next.lights[0].position, target.lights[0].position);
    }

    #[test]
    fn dropped_light_fades_out() {
        let (shown, target) = (lighting(0.2, &[light(0.0, 1.0)]), lighting(0.2, &[light(100.0, 1.0)]));
        let mut next = blend_lighting(&shown, &target, 0.25);
        assert_eq!(next.count, 2);
        assert!((next.lights[0].color.x - 0.25).abs() < 1e-6);
        assert!((next.lights[1].color.x - 0.75).abs() < 1e-6);
        assert_eq!(next.lights[1].position, shown.lights[0].position);
        for _ in 0..100 {
            next = blend_lighting(&next, &target, 0.25);
        }
        assert_eq!(next, target);
    }
}
