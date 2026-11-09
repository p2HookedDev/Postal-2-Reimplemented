pub use crate::components::{CanvasDrawn, FollowActor, OVERLAY_LAYERS, SKY_LAYER};
pub use crate::shown_scene::ShownScene;
pub use crate::components::{ActorModel, SceneView};
pub(crate) use crate::components::CanvasLayer;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
use crate::render::world_pass::{WorldPlacement, WorldSeenZones, WorldTargets};
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
use bevy::ecs::prelude::Mut;
use bevy::ecs::system::{EntityCommands, SystemParam};
use bevy::image::{CompressedImageFormatSupport, CompressedImageFormats, Image};
use bevy::light::cluster::{ClusterConfig, ClusterZConfig, GlobalClusterSettings};
use bevy::light::{AmbientLight, CascadeShadowConfig, ClusteredDecal};
use bevy::material::AlphaMode;
use bevy::math::{Affine2, Mat3, Quat, UVec2, Vec2, Vec3, Vec4};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::render::render_resource::Face;
use bevy::time::Time;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_math::{Rotator, Vector};
use p2_assets::{Blending, RenderState, UvModifier, uv_matrix};
use p2_bevy_assets::{MODULATE_2X, rotator_matrix, to_bevy};

use crate::render::actor_light::{ActorLit, LightEnvironment, animate_actor_speculars, light_actors};
use crate::actors::ZoneFog;
use crate::animation::FollowBone;
use crate::render::blended::{BlendedMaterial, OpaqueDecals, TerrainLayer, TerrainLayerMaterial};
use crate::render::lightmapped::{BakedLight, LightmappedMaterial};
use crate::render::mesh_light::{DynamicLit, MeshLitMaterial};
use crate::render::modern::{CASTER_LAYER, SunShadow};
use crate::render::shader_layers::Layers;
use crate::render::terrain_material::{TerrainLayers, TerrainMaterial, TerrainPassLayer};
use crate::display::{MODERN_AO, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use p2_render_cpu::{AntiportalActor, Plane, VisibleZones, ZonePortals};
use p2_vm::ObjectId;
mod sections;
mod materials;
mod camera;
mod animated_materials;
mod restyle;
mod fog;
mod zone_culling;
pub use sections::{MaterialImages, SectionView, Skin, SkinJoint, spawn_actor_sections};
use materials::{SectionMaterials, scene_section_material};
pub use materials::{ActorMaterialKey, SectionMaterial, follower_material, section_material, vertex_lit};
pub(crate) use materials::{SceneLights, actor_standard, alpha_key, cached_actor_material};
use camera::{SceneCameras, place_camera, place_overlay_cameras, plain_output, show_canvas_layers};
pub use camera::{NEAR_CLIPPING_PLANE, view_rotation};
use animated_materials::{AnimatedMaterials, MovingUv, animate_materials};
pub(crate) use animated_materials::set_uv_transform;
pub use restyle::{SwitchedLook, restyle_switched};
use fog::zone_fog;
use zone_culling::{follower_zones, zone_visibility};
pub(crate) use zone_culling::{bounds, merged_box};

fn from_bevy(v: Vec3) -> [f32; 3] {
    to_bevy([v.x, v.y, v.z])
}

pub struct MeshViewPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MeshViewSystems {
    Apply,
    Control,
    Camera,
}

impl Plugin for MeshViewPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            crate::render::particle_material::ParticlePlugin,
            crate::render::blended::BlendedPlugin,
            crate::render::lightmapped::LightmappedPlugin,
            crate::render::actor_light::ActorLightPlugin,
            crate::render::mesh_light::MeshLightPlugin,
            crate::render::world_pass::WorldPassPlugin,
            crate::render::sampler_refresh::SamplerRefreshPlugin,
        ));
        app.init_resource::<MeshView>()
            .init_resource::<ShownScene>()
            .init_resource::<ShadowOnly>()
            .init_resource::<LevelCollision>()
            .init_resource::<crate::render::bsp_light::FrameLights>()
            .init_resource::<AnimatedMaterials>()
            .init_resource::<crate::render::wet::WetTextures>()
            .add_systems(Update, crate::render::wet::animate_wet)
            .configure_sets(Update, (MeshViewSystems::Apply, MeshViewSystems::Control, MeshViewSystems::Camera).chain())
            .configure_sets(Update, crate::play::PlaySystems::Input.after(MeshViewSystems::Apply))
            .configure_sets(Update, crate::play::PlaySystems::Tick.before(MeshViewSystems::Camera))
            .configure_sets(Update, crate::render::modern::ModernGraphicsSystems::Apply.after(MeshViewSystems::Camera))
            .add_systems(Startup, (setup, steady_decal_order))
            .add_systems(Update, (apply_change, animate_materials, animate_actor_speculars, follow_msaa).chain().in_set(MeshViewSystems::Apply))
            .add_systems(Update, (place_camera, zone_fog, follower_zones, zone_visibility, light_actors::<crate::play::Play>, crate::render::terrain_light::light_terrain, crate::render::bsp_light::dynamic_lights).chain().in_set(MeshViewSystems::Camera))
            // After the HUD paint (egui's pass), so a layer shows in the frame it was painted.
            .add_systems(PostUpdate, show_canvas_layers.after(EguiPostUpdateSet::EndPass))
            .add_systems(PostUpdate, place_overlay_cameras.after(crate::play::PlaySystems::Present).before(CameraUpdateSystems).before(bevy::transform::TransformSystems::Propagate));
    }
}

#[derive(Resource)]
pub struct MeshView {
    pub viewport: Option<(UVec2, UVec2)>,
    /// Horizontal fields of view in degrees, as Unreal gives them: the view's (0: 90), and one per actor the HUD drew
    /// this frame (`DrawActor`), in call order (0: the view's); each is drawn by the overlay camera of its slot.
    pub fov: f32,
    pub overlays: Vec<f32>,
    pub occlusion_culling: bool,
    pub(crate) canvas_layers: Vec<Entity>,
    pub(crate) canvas_below: [bool; OVERLAY_LAYERS.len()],
    pending: Option<Change>,
}

impl Default for MeshView {
    fn default() -> Self {
        Self {
            viewport: None,
            fov: 0.0,
            overlays: Vec::new(),
            occlusion_culling: false,
            canvas_layers: Vec::new(),
            canvas_below: [false; OVERLAY_LAYERS.len()],
            pending: None,
        }
    }
}

/// Overlapping clustered decals blend in the order their cluster lists them, and that blend is not commutative. Bevy's
/// GPU clustering fills the lists with atomics in an order that changes every frame, so overlaps flicker tile by tile;
/// CPU clustering lists them in a fixed order.
fn steady_decal_order(settings: Option<ResMut<GlobalClusterSettings>>) {
    if let Some(mut settings) = settings {
        settings.gpu_clustering = None;
    }
}

/// The 3D cameras draw into one target, so they must agree. Off unless set, as Bevy's default 4× costs every pass (the
/// depth prepass included); a host without settings (the asset viewer) keeps it off. Ambient occlusion needs it off.
fn follow_msaa(settings: Option<Res<Settings>>, mut cameras: Query<&mut Msaa, With<Camera3d>>) {
    let Some(settings) = settings.filter(|settings| settings.is_changed()) else { return };
    let wanted = if settings.modern_effect(MODERN_AO) { Msaa::Off } else { Msaa::from_samples(settings.msaa_samples()) };
    for mut msaa in &mut cameras {
        msaa.set_if_neq(wanted);
    }
}

pub struct ModelView {
    pub sections: Vec<SectionView>,
    pub instances: Vec<Transform>,
    pub zone: Option<ZoneCulled>,
}

#[derive(Component, Clone)]
pub struct ZoneCulled {
    pub zones: Vec<u8>,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub only_from: Option<Vec<u8>>,
}

#[derive(Resource, Default)]
pub struct ShadowOnly(pub HashSet<Entity>);

pub struct MovingModel<F> {
    pub sections: Vec<SectionView>,
    pub transform: Transform,
    pub follow: F,
    pub lit: Option<ActorLit>,
}

pub struct Scene {
    pub models: Vec<ModelView>,
    pub center: Vec3,
    pub radius: f32,
    pub sky: Option<Sky>,
    pub start: Option<(Vec3, i32)>,
    pub collision: Option<StaticCollision>,
    pub fogs: Vec<Option<ZoneFog>>,
    pub decals: Vec<(Handle<Image>, Transform, u32)>,
    pub actors: Vec<MovingModel<FollowActor>>,
    pub attachments: Vec<MovingModel<FollowBone>>,
    pub lighting: Option<Arc<LightEnvironment>>,
    pub antiportals: Vec<AntiportalActor>,
    pub light_maps: Vec<Option<Handle<Image>>>,
}

/// From the `SkyZoneInfo`'s location, turned by its rotation and then the view's; only what lies in
/// that zone (unverified).
pub struct Sky {
    pub position: Vec3,
    pub rotation: Quat,
    pub models: Vec<ModelView>,
}

impl ShownScene {
    pub fn start_view(&self) -> Option<SceneView> {
        // Unreal yaw θ looks along (cos θ, sin θ) in X / Y, which is Bevy (cos θ, 0, sin θ); the camera looks along −Z
        // turned by its yaw, so its yaw is −θ − 90°.
        self.start.map(|(position, yaw)| SceneView {
            position,
            rotation: Quat::from_rotation_y(-(yaw as f32) * std::f32::consts::TAU / 65536.0 - std::f32::consts::FRAC_PI_2),
        })
    }
}

enum Change {
    Show(Box<Scene>),
    Clear,
}

impl MeshView {
    fn view_fov(&self) -> f32 {
        if self.fov > 0.0 { self.fov } else { 90.0 }
    }

    fn set_fov(&self, projection: &mut Mut<Projection>, degrees: f32) {
        let aspect = self.viewport.map_or(1.0, |(_, size)| size.x as f32 / size.y.max(1) as f32);
        let fov = 2.0 * ((degrees.to_radians() / 2.0).tan() / aspect).atan();
        if matches!(&**projection, Projection::Perspective(perspective) if perspective.fov != fov)
            && let Projection::Perspective(perspective) = &mut **projection
        {
            perspective.fov = fov;
        }
    }

    fn bevy_viewport(&self) -> Option<Viewport> {
        self.viewport.map(|(physical_position, physical_size)| Viewport { physical_position, physical_size, ..Default::default() })
    }

    pub fn show(&mut self, scene: Scene) {
        self.pending = Some(Change::Show(Box::new(scene)));
    }

    pub fn clear(&mut self) {
        self.pending = Some(Change::Clear);
    }
}

impl SceneView {
    pub fn look_from(location: Vector, rotation: Rotator, origin: Vec3) -> Self {
        Self { position: Vec3::from(to_bevy(location)) + origin, rotation: view_rotation(rotation) }
    }
}

#[derive(Component)]
struct SkyCamera {
    position: Option<(Vec3, Quat)>,
}

#[derive(Component)]
pub(crate) struct MapModel;

/// Retail renders each `DrawActor` call as a scene of its own and clears the depth only with `ClearZ`
/// (or `DrawActor(None, false, true)`), so a call without it is depth-tested against what came before.
#[derive(Component)]
struct OverlayCamera(usize);

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
        Camera { is_active: false, ..Default::default() },
        Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 500_000.0, ..Default::default() }),
        Transform::default(),
        SceneView::default(),
        crate::render::world_pass::WorldCamera,
        // Assigning lights costs by the clusters they cover, on the main thread; a longer list per pixel costs far less.
        ClusterConfig::FixedZ { total: 1536, z_slices: 16, z_config: ClusterZConfig::default(), dynamic_resizing: true },
        DistanceFog { color: Color::NONE, ..Default::default() },
        crate::play::sound_listener(),
        AmbientLight { affects_lightmapped_meshes: false, ..Default::default() },
    ));
    commands.spawn((
        Camera3d::default(),
        plain_output(),
        AmbientLight { affects_lightmapped_meshes: false, ..Default::default() },
        Camera { is_active: false, order: -1, ..Default::default() },
        Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 500_000.0, ..Default::default() }),
        Transform::default(),
        SkyCamera { position: None },
        ClusterConfig::None,
        RenderLayers::layer(SKY_LAYER),
    ));
    for (slot, layer) in OVERLAY_LAYERS.into_iter().enumerate() {
        let order = 2 * slot as isize;
        let canvas = commands
            .spawn((
                // A 3D camera that draws nothing 3D (`RenderLayers::none()`), only its egui layer: a `Camera2d` made its
                // depth texture single-sampled while the shared target was multisampled (`follow_msaa`), and wgpu refused
                // the pass (else opening the map screen crashed).
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
    mut view: ResMut<MeshView>,
    mut section_materials: SectionMaterials,
    map_models: Query<Entity, With<MapModel>>,
    cameras: SceneCameras,
    shown_level: ShownLevel,
    mut world: WorldTargets,
) {
    let SceneCameras { mut views, mut skies } = cameras;
    let ShownLevel { mut shown, mut level } = shown_level;
    let Some(change) = view.pending.take() else { return };
    world.clear();
    section_materials.animated.textures.clear();
    section_materials.animated.speculars.clear();
    *shown = ShownScene::default();
    *level = LevelCollision::default();
    for entity in &map_models {
        commands.entity(entity).despawn();
    }
    let Change::Show(scene) = change else {
        for mut sky_camera in &mut skies {
            sky_camera.position = None;
        }
        return;
    };
    let Scene { models, center, radius, sky, start, collision, fogs, decals, actors, attachments, lighting, antiportals, light_maps } = *scene;
    let (sky, sky_models) = sky.map_or((None, Vec::new()), |sky| (Some((sky.position - center, sky.rotation)), sky.models));
    for mut sky_camera in &mut skies {
        sky_camera.position = sky;
    }
    let portals = collision.as_ref().map(|collision| Arc::new(ZonePortals::new(&collision.model, antiportals)));
    let start = start.map(|(position, yaw)| (position - center, yaw));
    *level = LevelCollision(collision.map(Arc::new));
    *shown = ShownScene { fogs, origin: -center, portals, lighting, radius, start, light_maps };
    for mut view in &mut views {
        *view = shown.start_view().unwrap_or_default();
    }
    let mut cache = HashMap::new();
    // What does not move under one root, what moves under others: Bevy's transform propagation skips a tree none of
    // whose transforms changed, so the static scene costs nothing while actors move.
    let root = commands.spawn((MapModel, Transform::from_translation(-center), Visibility::default())).id();
    let attachments_root = commands.spawn((MapModel, Transform::from_translation(-center), Visibility::default())).id();
    let mut followers = HashMap::new();
    for MovingModel { sections, transform, follow, lit } in attachments {
        let material = |s: &SectionView| follower_material(s, &mut section_materials.materials, &mut section_materials.mesh_lit, &mut followers);
        spawn_actor_sections(&mut commands, attachments_root, sections, transform, (follow.actor, follow), material, lit, ());
    }
    for MovingModel { sections, transform, follow, lit } in actors {
        let parent = commands.spawn((MapModel, ActorModel(follow.actor), Transform::from_translation(-center), Visibility::default())).id();
        let material = |s: &SectionView| follower_material(s, &mut section_materials.materials, &mut section_materials.mesh_lit, &mut followers);
        spawn_actor_sections(&mut commands, parent, sections, transform, (follow.actor, follow), material, lit, ());
    }
    let world_supported = world.supported();
    commands.entity(root).with_children(|parent| {
        for (image, transform, tag) in decals {
            parent.spawn((ClusteredDecal { base_color_texture: Some(image), tag, ..Default::default() }, transform));
        }
        for (model, in_sky) in models.into_iter().map(|model| (model, false)).chain(sky_models.into_iter().map(|model| (model, true))) {
            let sections: Vec<_> = model
                .sections
                .into_iter()
                .map(|section| {
                    let material = scene_section_material(&section, center, &mut section_materials, &mut cache);
                    let lightmap = section.lightmap.map(|image| Lightmap {
                        image,
                        uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                        bicubic_sampling: false,
                    });
                    let world_chunks = section.chunks.filter(|_| !in_sky && world_supported && model.zone.as_ref().is_none_or(|zone| zone.only_from.is_none()) && matches!(section.alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_)));
                    (section.mesh, material, lightmap, world_chunks)
                })
                .collect();
            for instance in model.instances {
                for (mesh, material, lightmap, world_chunks) in &sections {
                    if let Some(chunks) = world_chunks {
                        let placement = WorldPlacement {
                            world_from_local: Transform::from_translation(-center).compute_affine() * instance.compute_affine(),
                            chunks: chunks.clone(),
                            zone: model.zone.as_ref().filter(|zone| zone.zones.len() == 1).map(|zone| zone.zones[0]),
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
                        entity.insert(RenderLayers::layer(SKY_LAYER));
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

#[cfg(test)]
mod tests;
