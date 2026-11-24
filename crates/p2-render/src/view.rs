pub use crate::components::{CASTER_LAYER, CanvasDrawn, FollowActor, MOVER_LAYER, OVERLAY_LAYERS, OverlaySlot, RESTING_PROP_LAYER, SKY_LAYER, STATIC_CASTER_LAYER, SUN_VIEW_LAYER, SunShadow, UNSEEN_LAYER};
pub use shown::ShownScene;
use crate::frame_facts::{FrameFacts, HudView};
use crate::level_collision::LevelCollision;
pub use crate::components::{ActorModel, BaseModel, SceneView};
pub(crate) use crate::components::CanvasLayer;
use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::entity::EntityHashSet;
use rustc_hash::FxHashMap;

use bevy::app::{App, Plugin, PostUpdate, Startup, Update};
use bevy::asset::{Assets, Handle, UntypedAssetId};
use bevy::camera::visibility::{RenderLayers, Visibility};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::render::occlusion_culling::OcclusionCulling;
use bevy::camera::{
    Camera, Camera3d, CameraUpdateSystems, ClearColorConfig, Exposure, PerspectiveProjection, Projection, Viewport,
};
use bevy_egui::{EguiContext, EguiContextSettings, EguiPostUpdateSet};
use bevy::math::Rect;
use crate::translucent_order::{SortedBspNode, TranslucentDraw};
use crate::world_pass::{WorldPlacement, WorldSeenZones, WorldTargets};
use bevy::pbr::{DistanceFog, FogFalloff, Lightmap, ScreenSpaceAmbientOcclusion};
use bevy::render::camera::MipBias;
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::color::{Alpha, Color, LinearRgba};
use bevy::camera::primitives::Aabb;
use bevy::ecs::prelude::{
    Bundle, Changed, ChildOf, Children, Commands, Component, Entity, Has, Or, Query, Res, ResMut, Resource, SystemSet,
    With, Without,
};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::prelude::resource_exists;
use bevy::ecs::prelude::{Added, Mut};
use bevy::ecs::message::Messages;
use bevy::ecs::system::{EntityCommands, SystemParam};
use bevy::image::{CompressedImageFormatSupport, CompressedImageFormats, Image};
use bevy::light::cluster::{ClusterConfig, ClusterZConfig, GlobalClusterSettings};
use bevy::light::{AmbientLight, CascadeShadowConfig, ClusteredDecal, ShadowFilteringMethod};
use bevy::render::RenderApp;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::material::AlphaMode;
use bevy::math::{Affine2, Mat3, Quat, UVec2, Vec2, Vec3, Vec4};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::Mesh3d;
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::render::render_resource::Face;
use bevy::time::Time;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_math::{Rotator, Vector};
use p2_assets::{UvModifier, uv_matrix};
use p2_bevy_assets::{MODULATE_2X, rotator_matrix, to_bevy};

use crate::actor_light::{ActorLit, animate_actor_speculars, settle_step};
use crate::components::FollowBone;
use crate::blended::{BlendedMaterial, OpaqueDecals, SkyStandard, SkyStandardMaterial, TerrainLayer, TerrainLayerMaterial};
use crate::lightmapped::{BakedLight, LightmappedMaterial};
use crate::mesh_light::{DynamicLit, MeshLitMaterial};
use crate::shader_layers::{Layers, ShaderLayers};
use crate::terrain_material::{TerrainLayers, TerrainMaterial};
use p2_host::display::Settings;
use p2_render_cpu::{Plane, VisibleZones, ZonePortals};
use p2_vm::ObjectId;
mod sections;
mod materials;
mod camera;
mod animated_materials;
mod restyle;
mod fog;
mod zone_culling;
mod sky_copies;
mod mover_sight;
mod shown;
pub use mover_sight::{MoverSightPlugin, MoversSeen};
pub use sections::spawn_actor_sections;
use materials::{SectionMaterials, scene_section_material};
pub use materials::{ActorLook, ActorMaterialCache, ActorMaterials, SectionMaterial, vertex_lit};
pub(crate) use materials::{alpha_key, cull_for};
pub(crate) use materials::image_size;
use camera::{SceneCameras, plain_output};
pub use camera::view_rotation;
use p2_vm::NEAR_CLIPPING_PLANE;
use animated_materials::{AnimatedMaterials, MovingUv, animate_materials};
pub(crate) use animated_materials::{set_specular_uv, set_uv_transform};
pub(crate) use restyle::{SwitchedLook, restyle_switched};
pub(crate) use restyle::rebind_materials;
#[cfg(test)]
use zone_culling::{follower_zones, layers_by_zone};
use crate::scene::{LayeredByZone, MovingModel, Scene, SceneChange, SectionView, Skin, SkinJoint, ZoneCulled, points_box, sky_and_world};
pub use zone_culling::{NoSunCast, SeenWithin};

pub struct MeshViewPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MeshViewSystems {
    Apply,
    Control,
    Camera,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SceneViewSystems {
    Place,
    Fog,
    Zones,
    Light,
    Overlays,
}

impl Plugin for MeshViewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MeshView>()
            .init_resource::<HudView>()
            .init_resource::<FrameFacts>()
            .add_message::<SceneChange>()
            .init_resource::<crate::components::OverlayLook>()
            .init_resource::<ShownScene>()
            .init_resource::<LevelCollision>()
            .init_resource::<AnimatedMaterials>()
            .init_resource::<ActorMaterialCache>()
            .init_resource::<crate::canvas::CanvasPainter>()
            .configure_sets(Update, (MeshViewSystems::Apply, MeshViewSystems::Control, MeshViewSystems::Camera).chain())
            .add_systems(Startup, (setup, steady_decal_order))
            .add_systems(Update, (apply_change, animate_materials, animate_actor_speculars, follow_msaa).chain().in_set(MeshViewSystems::Apply))
            .add_systems(
                PostUpdate,
                (
                    sky_copies::sky_copies,
                    (
                        sky_copies::follow_sky_material::<StandardMaterial>.run_if(resource_exists::<Assets<StandardMaterial>>),
                        sky_copies::follow_sky_material::<MeshLitMaterial>.run_if(resource_exists::<Assets<MeshLitMaterial>>),
                        sky_copies::follow_sky_material::<crate::actor_light::ActorLitMaterial>.run_if(resource_exists::<Assets<crate::actor_light::ActorLitMaterial>>),
                        sky_copies::follow_sky_material::<crate::particle_material::ParticleMaterial>.run_if(resource_exists::<Assets<crate::particle_material::ParticleMaterial>>),
                    ),
                )
                    .chain()
                    .before(bevy::transform::TransformSystems::Propagate),
            )
            .configure_sets(
                Update,
                (SceneViewSystems::Place, SceneViewSystems::Fog, SceneViewSystems::Zones, SceneViewSystems::Light).chain().in_set(MeshViewSystems::Camera),
            )
            .add_plugins((camera::ViewCameraPlugin, fog::FogPlugin, zone_culling::ZoneCullingPlugin, MoverSightPlugin, ExtractComponentPlugin::<OverlayCamera>::default()));
    }
}

#[derive(Resource)]
pub struct MeshView {
    pub viewport: Option<(UVec2, UVec2)>,
    pub occlusion_culling: bool,
    pub(crate) canvas_layers: Vec<Entity>,
    pub(crate) canvas_below: [bool; OVERLAY_LAYERS.len()],
    pub modern: crate::modern_view::ModernView,
}

impl Default for MeshView {
    fn default() -> Self {
        Self {
            viewport: None,
            occlusion_culling: false,
            canvas_layers: Vec::new(),
            canvas_below: [false; OVERLAY_LAYERS.len()],
            modern: Default::default(),
        }
    }
}

/// Overlapping clustered decals blend in cluster list order, which is not commutative. Bevy's GPU clustering
/// fills the lists with atomics in an order that changes every frame, so overlaps flicker; CPU clustering keeps a
/// fixed order.
fn steady_decal_order(settings: Option<ResMut<GlobalClusterSettings>>) {
    if let Some(mut settings) = settings {
        settings.gpu_clustering = None;
    }
}

/// All cameras, 2D ones included, draw into one target, so they must agree on MSAA. Off unless set, as Bevy's
/// default 4x costs every pass; ambient occlusion needs it off. Late-spawned cameras (the HUD's) take it too:
/// differing sample counts draw into different images, and the later write covers the earlier.
fn follow_msaa(settings: Option<Res<Settings>>, mut cameras: Query<&mut Msaa, With<Camera>>, added: Query<(), Added<Camera>>) {
    let Some(settings) = settings.filter(|settings| settings.is_changed() || !added.is_empty()) else { return };
    let wanted = Msaa::from_samples(settings.msaa_samples());
    for mut msaa in &mut cameras {
        msaa.set_if_neq(wanted);
    }
}

#[derive(Resource, Default)]
pub struct ShadowOnly(pub EntityHashSet);

impl ShownScene {
    pub fn start_view(&self) -> Option<SceneView> {
        // Unreal yaw t looks along (cos t, sin t) in X / Y, which is Bevy (cos t, 0, sin t); the camera looks along -Z
        // turned by its yaw, so its yaw is -t - 90 degrees.
        self.start.map(|(position, yaw)| SceneView {
            position,
            rotation: Quat::from_rotation_y(-p2_math::radians(yaw) - std::f32::consts::FRAC_PI_2),
        })
    }
}

impl MeshView {
    pub fn hide_canvas_layers(&mut self) {
        self.canvas_below = [false; OVERLAY_LAYERS.len()];
    }

    fn view_fov(&self, facts: &FrameFacts) -> f32 {
        match self.modern.photo_fov {
            Some(degrees) => f32::from(degrees),
            None if facts.fov > 0.0 => facts.fov,
            None => 90.0,
        }
    }

    fn vertical_fov(&self, degrees: f32) -> f32 {
        let aspect = self.bevy_viewport().map_or(1.0, |viewport| viewport.physical_size.x as f32 / viewport.physical_size.y.max(1) as f32);
        crate::modern_view::vertical_fov(degrees, aspect, self.modern.wide)
    }

    fn set_fov(&self, projection: &mut Mut<Projection>, degrees: f32) {
        let fov = self.vertical_fov(degrees);
        if matches!(&**projection, Projection::Perspective(perspective) if perspective.fov != fov)
            && let Projection::Perspective(perspective) = &mut **projection
        {
            perspective.fov = fov;
        }
    }

    fn bevy_viewport(&self) -> Option<Viewport> {
        self.viewport.map(|(position, size)| {
            let (physical_position, physical_size) = if self.modern.bars { crate::modern_view::centred_wide(position, size) } else { (position, size) };
            Viewport { physical_position, physical_size, ..Default::default() }
        })
    }

}

impl SceneView {
    pub fn look_from(location: Vector, rotation: Rotator, origin: Vec3) -> Self {
        Self { position: Vec3::from(to_bevy(location)) + origin, rotation: view_rotation(rotation) }
    }
}

#[derive(Component)]
struct SkyRoot;

#[derive(Component)]
pub struct MapModel;

/// Retail renders each `DrawActor` call as a scene of its own and clears depth only with `ClearZ`
/// (or `DrawActor(None, false, true)`), so a call without it is depth-tested against what came before.
#[derive(Component, Clone, ExtractComponent)]
#[extract_app(RenderApp)]
pub(crate) struct OverlayCamera(pub(crate) usize);

#[derive(Component)]
pub(crate) struct HudCamera;

pub fn hud_camera() -> impl Bundle {
    let (clear_color, output_mode) = hud_alone();
    (
        Camera3d::default(),
        plain_output(),
        Camera { order: 10, output_mode, clear_color, ..Default::default() },
        Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 10_000.0, ..Default::default() }),
        Transform::default(),
        RenderLayers::none(),
        ClusterConfig::Single,
        OverlayCamera(crate::overlay_route::HUD_SLOT),
        HudCamera,
        ShadowFilteringMethod::default(),
        AmbientLight { affects_lightmapped_meshes: false, ..Default::default() },
    )
}

fn hud_alone() -> (ClearColorConfig, bevy::camera::CameraOutputMode) {
    let output = bevy::camera::CameraOutputMode::Write {
        blend_state: Some(bevy::render::render_resource::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        clear_color: ClearColorConfig::None,
    };
    (ClearColorConfig::Custom(Color::NONE), output)
}

#[derive(Resource)]
struct MissingTextureMaterial(Handle<StandardMaterial>);

fn setup(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, formats: Option<Res<CompressedImageFormatSupport>>, mut view: ResMut<MeshView>) {
    if let Some(formats) = formats {
        p2_bevy_assets::set_block_compression(formats.0.contains(CompressedImageFormats::BC));
    }
    let material = StandardMaterial { base_color: Color::srgb(0.75, 0.75, 0.75), perceptual_roughness: 0.8, ..Default::default() };
    commands.insert_resource(MissingTextureMaterial(materials.add(material)));
    commands.spawn((
        Camera3d::default(),
        plain_output(),
        Camera { is_active: false, order: -1, ..Default::default() },
        Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 500_000.0, ..Default::default() }),
        Transform::default(),
        SceneView::default(),
        RenderLayers::from_layers(&[0, SKY_LAYER]),
        crate::world_pass::WorldCamera,
        // Assigning lights costs by the clusters they cover, on the main thread; a longer list per pixel costs far less.
        // Bevy's first slice of 5 is meant for metres: in Unreal units it wastes slices near the eye and leaves the
        // fight's range in deep slices, whose decal lists every pixel there walks.
        ClusterConfig::FixedZ {
            total: 1536,
            z_slices: 16,
            z_config: ClusterZConfig { first_slice_depth: 100.0, ..Default::default() },
            dynamic_resizing: true,
        },
        DistanceFog { color: Color::NONE, ..Default::default() },
        AmbientLight { affects_lightmapped_meshes: false, ..Default::default() },
    ));
    for (slot, layer) in OVERLAY_LAYERS.into_iter().enumerate() {
        let order = 2 * slot as isize;
        let canvas = commands
            .spawn((
                // A 3D camera that draws nothing 3D, only its egui layer: a `Camera2d` made its depth texture single-sampled
                // while the shared target was multisampled, and wgpu refused the pass, crashing on the map screen.
                Camera3d::default(),
                plain_output(),
                RenderLayers::none(),
                Camera { is_active: false, order: order + 1, clear_color: ClearColorConfig::None, ..Default::default() },
                EguiContext::default(),
                EguiContextSettings { enable_cursor_icon_updates: false, enable_ime: false, ..Default::default() },
                CanvasLayer(slot),
                ClusterConfig::None,
            ))
            .id();
        view.canvas_layers.push(canvas);
        commands.spawn((
            Camera3d::default(),
            plain_output(),
            Camera { is_active: false, order: order + 2, clear_color: ClearColorConfig::None, ..Default::default() },
            Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 10_000.0, ..Default::default() }),
            Transform::default(),
            RenderLayers::layer(layer),
            ClusterConfig::Single,
            OverlayCamera(slot),
            ShadowFilteringMethod::default(),
            AmbientLight { affects_lightmapped_meshes: false, ..Default::default() },
        ));
    }
}

#[derive(SystemParam)]
struct ShownLevel<'w> {
    shown: ResMut<'w, ShownScene>,
    level: ResMut<'w, LevelCollision>,
}

fn apply_change(
    mut commands: Commands,
    mut changes: ResMut<Messages<SceneChange>>,
    mut section_materials: SectionMaterials,
    map_models: Query<Entity, With<MapModel>>,
    cameras: SceneCameras,
    shown_level: ShownLevel,
    mut world: WorldTargets,
) {
    let SceneCameras { mut views } = cameras;
    let ShownLevel { mut shown, mut level } = shown_level;
    if changes.is_empty() {
        return;
    }
    let Some(change) = changes.drain().last() else { return };
    world.clear();
    section_materials.animated.textures.clear();
    section_materials.animated.speculars.clear();
    section_materials.actors.forget_followers();
    *shown = ShownScene::default();
    *level = LevelCollision::default();
    for entity in &map_models {
        commands.entity(entity).despawn();
    }
    let SceneChange::Show(scene) = change else { return };
    let Scene { models, center, radius, sky, start, collision, fogs, decals, actors, attachments, lighting, light_bakes, antiportals, light_maps } = *scene;
    let sky_zone = sky.as_ref().map(|sky| sky.zone);
    let (sky, sky_models) = sky.map_or((None, Vec::new()), |sky| (Some((sky.position - center, sky.rotation)), sky.models));
    let portals = collision.as_ref().map(|collision| Arc::new(ZonePortals::new(&collision.model, antiportals)));
    let start = start.map(|(position, yaw)| (position - center, yaw));
    *level = LevelCollision(collision.map(Arc::new));
    *shown = ShownScene { fogs, origin: -center, portals, lighting, light_bakes, radius, start, light_maps, sky_zone, sky_place: sky };
    for mut view in &mut views {
        *view = shown.start_view().unwrap_or_default();
    }
    let mut cache = FxHashMap::default();
    // Static things under one root, moving ones under others: Bevy's transform propagation skips a tree whose
    // transforms did not change, so the static scene costs nothing while actors move.
    let root = commands.spawn((MapModel, Transform::from_translation(-center), Visibility::default())).id();
    let attachments_root = commands.spawn((MapModel, Transform::from_translation(-center), Visibility::default())).id();
    let sky_root = commands.spawn((MapModel, SkyRoot, Transform::from_translation(-center), Visibility::default())).id();
    for MovingModel { sections, transform, follow, lit } in attachments {
        spawn_actor_sections(&mut commands, attachments_root, sections, transform, (follow.actor, follow), (&mut section_materials.actors, ActorLook::Follower), lit, ());
    }
    for MovingModel { sections, transform, follow, lit } in actors {
        let parent = commands.spawn((MapModel, ActorModel(follow.actor), Transform::from_translation(-center), Visibility::default())).id();
        spawn_actor_sections(&mut commands, parent, sections, transform, (follow.actor, follow), (&mut section_materials.actors, ActorLook::Follower), lit, ());
    }
    let world_supported = world.supported();
    commands.entity(root).with_children(|parent| {
        for (image, transform, tag, blood) in decals {
            let mut decal = parent.spawn((ClusteredDecal { base_color_texture: Some(image), tag, ..Default::default() }, transform));
            if blood {
                decal.insert(crate::wounds::BloodDecal);
            }
        }
        for (model, in_sky) in models.into_iter().map(|model| (model, false)).chain(sky_models.into_iter().map(|model| (model, true))) {
            let sections: Vec<_> = model
                .sections
                .into_iter()
                .map(|section| {
                    let material = scene_section_material(&section, center, sky.filter(|_| in_sky), &mut section_materials, &mut cache);
                    let lightmap = section.lightmap.map(|image| Lightmap {
                        image,
                        uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                        bicubic_sampling: false,
                    });
                    let world_chunks = section.chunks.filter(|_| !in_sky && world_supported && model.zone.as_ref().is_none_or(|zone| zone.only_from.is_none()) && matches!(section.alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_)));
                    let sort_node = section.bsp_node.and_then(|node| level.model()?.nodes.get(node).map(|bsp| SortedBspNode::new(node, bsp.plane, center)));
                    let terrain_layer = section.terrain_layer.map(TranslucentDraw::Terrain);
                    (section.mesh, material, lightmap, world_chunks, sort_node, terrain_layer)
                })
                .collect();
            for instance in model.instances {
                for (mesh, material, lightmap, world_chunks, sort_node, terrain_layer) in &sections {
                    if let Some(chunks) = world_chunks {
                        let placement = WorldPlacement {
                            world_from_local: Transform::from_translation(-center).compute_affine() * instance.compute_affine(),
                            chunks: chunks.clone(),
                            casts: model.casts,
                            zones: model.zone.as_ref().map(|zone| zone.zones.clone()).filter(|zones| !zones.is_empty()),
                        };
                        world.add(mesh, material, lightmap.as_ref().map(|lightmap| &lightmap.image), placement);
                        continue;
                    }
                    let mut entity = parent.spawn((Mesh3d(mesh.clone()), instance));
                    material.insert(&mut entity);
                    if let Some(lightmap) = lightmap {
                        entity.insert(lightmap.clone());
                    }
                    if in_sky {
                        entity.insert((RenderLayers::layer(SKY_LAYER), ChildOf(sky_root)));
                    }
                    if !model.casts {
                        entity.insert(NoSunCast);
                    }
                    if let Some(sort_node) = sort_node {
                        entity.insert(*sort_node);
                    }
                    if let Some(terrain_layer) = terrain_layer {
                        entity.insert(*terrain_layer);
                    }
                    if let Some(zone) = &model.zone {
                        entity.insert(zone.clone());
                    }
                }
            }
        }
    });
    world.finish();
}

pub fn two_zone_model() -> p2_assets::Model {
    let node = p2_assets::BspNode {
        plane: [0.0, 0.0, 1.0, 0.0],
        node_flags: 0,
        vert_pool: 0,
        surface: 0,
        back: None,
        front: None,
        coplanar: None,
        collision_bound: None,
        render_bound: None,
        zones: [1, 2],
        num_vertices: 0,
        leaves: [0, 1],
        section: None,
        first_vertex: 0,
        light_map: None,
    };
    p2_assets::Model {
        vectors: Vec::new(),
        points: Vec::new(),
        nodes: vec![node],
        surfaces: Vec::new(),
        verts: Vec::new(),
        num_shared_sides: 0,
        zones: Vec::new(),
        polys: 0,
        bounds: Vec::new(),
        leaf_hulls: Vec::new(),
        leaves: [1, 2].map(|zone| p2_assets::Leaf { zone, permeating: 0, volumetric: 0, visible_zones: 0 }).to_vec(),
        lights: Vec::new(),
        root_outside: true,
        linked: false,
        sections: Vec::new(),
        light_maps: Vec::new(),
        light_map_textures: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
