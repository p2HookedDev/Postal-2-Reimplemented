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
use bevy::camera::{Camera, Camera3d, CameraUpdateSystems, ClearColorConfig, Exposure, PerspectiveProjection, Projection, Viewport};
use bevy_egui::{EguiContext, EguiContextSettings, EguiPostUpdateSet};
use bevy::math::Rect;
use crate::render::world_pass::{WorldPlacement, WorldSeenZones, WorldTargets};
use bevy::pbr::{DistanceFog, FogFalloff, Lightmap, ScreenSpaceAmbientOcclusion};
use bevy::render::camera::MipBias;
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::color::{Alpha, Color, LinearRgba};
use bevy::camera::primitives::Aabb;
use bevy::ecs::prelude::{Bundle, Changed, ChildOf, Children, Commands, Component, Entity, Has, Or, Query, Res, ResMut, Resource, SystemSet, With, Without};
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
use crate::display::Settings;
use crate::level_collision::{LevelCollision, StaticCollision};
use p2_render_cpu::{AntiportalActor, Plane, VisibleZones, ZonePortals};
use p2_vm::ObjectId;

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
    let wanted = if settings.modern_ao() { Msaa::Off } else { Msaa::from_samples(settings.msaa_samples()) };
    for mut msaa in &mut cameras {
        msaa.set_if_neq(wanted);
    }
}

#[derive(Clone)]
pub struct MaterialImages {
    pub image: Handle<Image>,
    pub glow: Option<Handle<Image>>,
    pub detail: Option<Handle<Image>>,
    pub specular: Option<(Handle<Image>, crate::render::shader_layers::SpecularParams)>,
    pub state: RenderState,
}

#[derive(Clone)]
pub struct SectionView {
    pub mesh: Handle<Mesh>,
    pub texture: Option<Handle<Image>>,
    pub glow: Option<Handle<Image>>,
    pub layers: Layers,
    pub alpha_mode: AlphaMode,
    pub no_scene_lights: bool,
    pub fullbright: bool,
    pub depth_bias: f32,
    pub lightmap: Option<Handle<Image>>,
    pub two_sided: bool,
    pub uv: Vec<UvModifier>,
    pub skin: Option<Skin>,
    pub tint: [f32; 4],
    pub alpha_map: Option<Handle<Image>>,
    pub terrain: Vec<TerrainPassLayer>,
    pub chunks: Option<Arc<[crate::render::world_pass::Chunk]>>,
}

#[derive(Clone)]
pub struct Skin {
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    pub bind: Arc<[Transform]>,
}

impl SectionView {
    pub fn textured(mesh: Handle<Mesh>, found: Option<MaterialImages>) -> Self {
        let state = found.as_ref().map(|found| found.state.clone()).unwrap_or_default();
        let blending = match state.blending.unwrap_or(Blending::Opaque) {
            Blending::Opaque | Blending::Masked if state.color_blend => Blending::Alpha,
            blending => blending,
        };
        let alpha_mode = match blending {
            Blending::Opaque => AlphaMode::Opaque,
            // Texels pass above the reference: half a step over it keeps an alpha equal to it out.
            Blending::Masked => AlphaMode::Mask((f32::from(state.alpha_ref.unwrap_or(127)) + 0.5) / 255.0),
            Blending::Alpha => AlphaMode::Blend,
            Blending::Additive => AlphaMode::Add,
            Blending::Modulate => AlphaMode::Multiply,
            Blending::Invisible => AlphaMode::Mask(2.0),
        };
        Self {
            mesh,
            glow: found.as_ref().and_then(|found| found.glow.clone()),
            layers: Layers::new(
                found.as_ref().and_then(|found| found.detail.clone()),
                state.detail.map_or(0.0, |detail| detail.scale),
                found.as_ref().and_then(|found| found.specular.clone()),
                state.specular_uv.clone(),
                state.opacity.is_some() && (state.glow.is_some() || state.specular.is_some()),
            ),
            texture: found.map(|found| found.image),
            alpha_mode,
            no_scene_lights: state.unlit,
            fullbright: state.unlit,
            depth_bias: 0.0,
            lightmap: None,
            two_sided: state.two_sided,
            uv: state.uv,
            skin: None,
            tint: state.color.map_or([1.0; 4], |color| color.map(|c| (f32::from(c) / 255.0).powf(2.2))),
            alpha_map: None,
            terrain: Vec::new(),
            chunks: None,
        }
    }

    pub fn emissive(&self) -> LinearRgba {
        if self.glow.is_some() { LinearRgba::WHITE } else { LinearRgba::BLACK }
    }

    pub fn material_key(&self) -> String {
        format!(
            "{:?} {:?} {} {:?} {} {} {:?} {} {:?} {:?} {:?} {:?}",
            self.texture.as_ref().map(Handle::id),
            self.glow.as_ref().map(Handle::id),
            self.layers.key(),
            self.alpha_mode,
            self.no_scene_lights,
            self.depth_bias,
            self.lightmap.is_some(),
            self.two_sided,
            self.uv,
            self.tint,
            self.alpha_map.as_ref().map(Handle::id),
            self.terrain
        )
    }
}

#[derive(Component, Clone, Copy)]
pub struct SkinJoint {
    pub actor: ObjectId,
    pub bone: usize,
}

pub(crate) fn actor_standard(section: &SectionView, unlit: bool) -> StandardMaterial {
    let [r, g, b, a] = section.tint;
    StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_texture: section.texture.clone(),
        emissive: section.emissive(),
        emissive_texture: section.glow.clone(),
        alpha_mode: section.alpha_mode,
        unlit,
        cull_mode: if section.two_sided { None } else { Some(Face::Back) },
        double_sided: section.two_sided,
        ..Default::default()
    }
}

pub(crate) fn alpha_key(mode: AlphaMode) -> (std::mem::Discriminant<AlphaMode>, u32) {
    (std::mem::discriminant(&mode), if let AlphaMode::Mask(cutoff) = mode { cutoff.to_bits() } else { 0 })
}

#[derive(PartialEq, Eq, Hash)]
pub struct ActorMaterialKey {
    texture: Option<bevy::asset::AssetId<Image>>,
    layers: String,
    alpha: (std::mem::Discriminant<AlphaMode>, u32),
    no_scene_lights: bool,
    two_sided: bool,
    tint: [u32; 4],
    vertex_lit: bool,
}

pub(crate) fn cached_actor_material(
    cache: &mut HashMap<ActorMaterialKey, SectionMaterial>,
    section: &SectionView,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
    standard: impl FnOnce() -> StandardMaterial,
) -> SectionMaterial {
    let key = ActorMaterialKey {
        texture: section.texture.as_ref().map(Handle::id),
        layers: section.layers.key(),
        alpha: alpha_key(section.alpha_mode),
        no_scene_lights: section.no_scene_lights,
        two_sided: section.two_sided,
        tint: section.tint.map(f32::to_bits),
        vertex_lit: vertex_lit(section),
    };
    cache.entry(key).or_insert_with(|| section_material(section, standard(), materials, mesh_lit)).clone()
}

pub fn follower_material(
    section: &SectionView,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
    cache: &mut HashMap<ActorMaterialKey, SectionMaterial>,
) -> SectionMaterial {
    cached_actor_material(cache, section, materials, mesh_lit, || StandardMaterial { perceptual_roughness: 0.8, ..actor_standard(section, section.no_scene_lights) })
}

#[expect(clippy::too_many_arguments, reason = "one call per kind of actor model; the parts differ per caller")]
pub fn spawn_actor_sections(
    commands: &mut Commands,
    parent: Entity,
    sections: Vec<SectionView>,
    transform: Transform,
    (actor, follow): (ObjectId, impl Component + Clone),
    mut material: impl FnMut(&SectionView) -> SectionMaterial,
    lit: Option<ActorLit>,
    extra: impl Bundle + Clone,
) {
    let mut skin: Option<SkinnedMesh> = None;
    for section in sections {
        let mut spawned = commands.spawn((Mesh3d(section.mesh.clone()), transform, follow.clone(), extra.clone(), ChildOf(parent)));
        material(&section).insert(&mut spawned);
        let entity = spawned.id();
        if let Some(lit) = &lit {
            commands.entity(entity).insert(lit.clone().with_layers(section.layers.clone()));
        }
        let Some(Skin { inverse_bindposes, bind }) = section.skin.clone() else { continue };
        let skinned = skin.get_or_insert_with(|| {
            let joints = (bind.iter().enumerate())
                .map(|(bone, &at)| commands.spawn((at, SkinJoint { actor, bone }, ChildOf(entity))).id())
                .collect();
            SkinnedMesh { inverse_bindposes, joints }
        });
        // Bounds follow the posed joints (frustum and occlusion culling), not the bind pose: recomputed when the
        // actor's pose changes (`animation::skinned_bounds`), not every frame as Bevy's `DynamicSkinnedMeshBounds`.
        commands.entity(entity).insert(skinned.clone());
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

/// The Bevy camera rotation of an Unreal view rotation (pitch, yaw, roll; 65536 a turn): Unreal looks
/// along X with Z up and Y right, a Bevy camera along −Z with Y up and X right, Y and Z swapped.
pub fn view_rotation(rotation: Rotator) -> Quat {
    let matrix = rotator_matrix(rotation);
    let axis = |i: usize| {
        let v = matrix.col(i);
        Vec3::new(v.x, v.z, v.y)
    };
    let (forward, right, up) = (axis(0), axis(1), axis(2));
    Quat::from_mat3(&Mat3::from_cols(right, up, -forward)).normalize()
}

#[derive(Component)]
struct SkyCamera {
    position: Option<(Vec3, Quat)>,
}

#[derive(Component)]
pub(crate) struct MapModel;

/// Unreal units. Retail uses 5 for every scene: the view, the sky and each `DrawActor`.
pub const NEAR_CLIPPING_PLANE: f32 = 5.0;

/// Retail renders each `DrawActor` call as a scene of its own and clears the depth only with `ClearZ`
/// (or `DrawActor(None, false, true)`), so a call without it is depth-tested against what came before.
#[derive(Component)]
struct OverlayCamera(usize);

/// `DefaultTexMipBias` as `Postal2.ini` ships it.
const DEFAULT_MIP_BIAS: f32 = -0.5;

fn plain_output() -> (Msaa, Tonemapping, DebandDither, MipBias) {
    (Msaa::Off, Tonemapping::None, DebandDither::Disabled, MipBias(DEFAULT_MIP_BIAS))
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

#[derive(Clone, PartialEq)]
pub enum SectionMaterial {
    Standard(Handle<StandardMaterial>),
    Blended(Handle<BlendedMaterial>),
    Terrain(Handle<TerrainLayerMaterial>),
    TerrainPass(Handle<TerrainMaterial>),
    Lightmapped(Handle<LightmappedMaterial>),
    MeshLit(Handle<MeshLitMaterial>),
}

impl SectionMaterial {
    pub(crate) fn id(&self) -> UntypedAssetId {
        match self {
            Self::Standard(material) => material.id().untyped(),
            Self::Blended(material) => material.id().untyped(),
            Self::Terrain(material) => material.id().untyped(),
            Self::TerrainPass(material) => material.id().untyped(),
            Self::Lightmapped(material) => material.id().untyped(),
            Self::MeshLit(material) => material.id().untyped(),
        }
    }

    pub(crate) fn insert(&self, entity: &mut EntityCommands) {
        match self {
            Self::Standard(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::Blended(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::Terrain(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::TerrainPass(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::Lightmapped(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::MeshLit(material) => entity.insert(MeshMaterial3d(material.clone())),
        };
    }
}

pub fn vertex_lit(section: &SectionView) -> bool {
    section.no_scene_lights && !section.fullbright && section.skin.is_none()
}

pub fn section_material(
    section: &SectionView,
    standard: StandardMaterial,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
) -> SectionMaterial {
    if vertex_lit(section) {
        SectionMaterial::MeshLit(mesh_lit.add(MeshLitMaterial { base: section.layers.lit(standard), extension: DynamicLit::new(&section.layers) }))
    } else {
        SectionMaterial::Standard(materials.add(standard))
    }
}

#[derive(SystemParam)]
struct SectionMaterials<'w> {
    grey: Res<'w, MissingTextureMaterial>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    blended: ResMut<'w, Assets<BlendedMaterial>>,
    terrain_layers: ResMut<'w, Assets<TerrainLayerMaterial>>,
    terrain_passes: ResMut<'w, Assets<TerrainMaterial>>,
    lightmapped: ResMut<'w, Assets<LightmappedMaterial>>,
    mesh_lit: ResMut<'w, Assets<MeshLitMaterial>>,
    images: Res<'w, Assets<Image>>,
    animated: ResMut<'w, AnimatedMaterials>,
}

#[derive(SystemParam)]
struct SceneCameras<'w, 's> {
    views: Query<'w, 's, &'static mut SceneView>,
    skies: Query<'w, 's, &'static mut SkyCamera>,
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

fn scene_section_material(section: &SectionView, center: Vec3, stores: &mut SectionMaterials, cache: &mut HashMap<(bool, String), SectionMaterial>) -> SectionMaterial {
    let Some(texture) = &section.texture else { return SectionMaterial::Standard(stores.grey.0.clone()) };
    let vertex_lit = vertex_lit(section);
    let key = (vertex_lit, section.material_key());
    if let Some(material) = cache.get(&key) {
        return material.clone();
    }
    let [r, g, b, a] = section.tint;
    let standard = StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_texture: Some(texture.clone()),
        emissive: section.emissive(),
        emissive_texture: section.glow.clone(),
        alpha_mode: section.alpha_mode,
        perceptual_roughness: 0.8,
        unlit: section.no_scene_lights && section.lightmap.is_none(),
        depth_bias: section.depth_bias,
        reflectance: if section.lightmap.is_some() { 0.0 } else { 0.5 },
        lightmap_exposure: MODULATE_2X / Exposure::default().exposure(),
        cull_mode: if section.two_sided { None } else { Some(Face::Back) },
        double_sided: section.two_sided,
        ..Default::default()
    };
    let material = if !section.terrain.is_empty() {
        let base = StandardMaterial { base_color_texture: None, ..standard };
        SectionMaterial::TerrainPass(stores.terrain_passes.add(TerrainMaterial { base, extension: TerrainLayers::new(&section.terrain, center) }))
    } else if let Some(alpha_map) = &section.alpha_map {
        let extension = TerrainLayer::new(alpha_map.clone(), &section.layers);
        SectionMaterial::Terrain(stores.terrain_layers.add(TerrainLayerMaterial { base: section.layers.lit(standard), extension }))
    } else if section.lightmap.is_some() {
        SectionMaterial::Lightmapped(stores.lightmapped.add(LightmappedMaterial { base: section.layers.lit(standard), extension: BakedLight::new(&section.layers) }))
    } else if vertex_lit {
        SectionMaterial::MeshLit(stores.mesh_lit.add(MeshLitMaterial { base: section.layers.lit(standard), extension: DynamicLit::new(&section.layers) }))
    } else if section.alpha_mode == AlphaMode::Blend && section.uv.is_empty() {
        SectionMaterial::Blended(stores.blended.add(BlendedMaterial { base: section.layers.lit(standard), extension: OpaqueDecals::new(&section.layers) }))
    } else {
        SectionMaterial::Standard(stores.materials.add(standard))
    };
    let size = |image: &Handle<Image>| stores.images.get(image).map_or([1.0, 1.0], |image| image.size().as_vec2().to_array());
    if !section.uv.is_empty() {
        let moving = MovingUv { material: material.clone(), modifiers: section.uv.clone(), size: size(texture) };
        stores.animated.textures.push(moving);
    }
    if let Some(specular) = section.layers.flat_specular.as_ref().filter(|_| !section.layers.specular_uv.is_empty()) {
        let moving = MovingUv { material: material.clone(), modifiers: section.layers.specular_uv.clone(), size: size(specular) };
        stores.animated.speculars.push(moving);
    }
    cache.insert(key, material.clone());
    material
}

fn set_camera(camera: &mut Mut<Camera>, is_active: bool, viewport: Option<&Viewport>) {
    if camera.is_active != is_active {
        camera.is_active = is_active;
    }
    let same = match (camera.viewport.as_ref(), viewport) {
        (None, None) => true,
        (Some(a), Some(b)) => a.physical_position == b.physical_position && a.physical_size == b.physical_size && a.depth == b.depth,
        _ => false,
    };
    if !same {
        camera.viewport = viewport.cloned();
    }
}

/// After the frame is presented: a slot's camera turns on and takes its field of view in the frame its models move to its
/// layer (a kick's foot takes slot 0 and the weapon moves to slot 1), not a frame later.
fn place_overlay_cameras(
    view: Res<MeshView>,
    cameras: Query<&Transform, (With<SceneView>, Without<OverlayCamera>)>,
    mut overlays: Query<(&mut Camera, &mut Transform, &mut Projection, &OverlayCamera), Without<SceneView>>,
) {
    let viewport = view.bevy_viewport();
    let view_transform = cameras.iter().next().copied();
    for (mut camera, mut transform, mut projection, OverlayCamera(slot)) in &mut overlays {
        let fov = view.overlays.get(*slot).copied();
        view.set_fov(&mut projection, fov.filter(|&fov| fov > 0.0).unwrap_or(view.view_fov()));
        set_camera(&mut camera, viewport.is_some() && fov.is_some(), viewport.as_ref());
        if let Some(view) = view_transform
            && *transform != view
        {
            *transform = view;
        }
    }
}

fn show_canvas_layers(view: Res<MeshView>, mut layers: Query<(&mut Camera, &CanvasLayer)>) {
    for (mut camera, CanvasLayer(slot)) in &mut layers {
        let shown = view.canvas_below.get(*slot).copied().unwrap_or(false);
        if camera.is_active != shown {
            camera.is_active = shown;
        }
    }
}

fn place_camera(
    mut commands: Commands,
    view: Res<MeshView>,
    mut cameras: Query<
        (Entity, &mut Camera, &mut Transform, &SceneView, &mut Projection, Has<OcclusionCulling>, Has<ScreenSpaceAmbientOcclusion>),
        Without<SkyCamera>,
    >,
    mut skies: Query<(&mut Camera, &mut Transform, &SkyCamera, &mut Projection), Without<SceneView>>,
) {
    let view_fov = view.view_fov();
    let viewport = view.bevy_viewport();
    let has_sky = skies.iter().any(|(_, _, sky, _)| sky.position.is_some());
    let mut rotation = Quat::IDENTITY;
    for (entity, mut camera, mut transform, scene_view, mut projection, culled, ssao) in &mut cameras {
        view.set_fov(&mut projection, view_fov);
        set_camera(&mut camera, viewport.is_some(), viewport.as_ref());
        // GPU occlusion culling (Bevy 0.20) takes the whole depth texture for the view's: only for a view filling its
        // target (the game's full window; its size is rounded from egui points), not one beside panels (the viewer).
        let fills = view.occlusion_culling
            && viewport.as_ref().is_none_or(|v| {
                let target = camera.physical_target_size();
                v.physical_position == UVec2::ZERO && target.is_some_and(|size| (size.as_ivec2() - v.physical_size.as_ivec2()).abs().max_element() <= 2)
            });
        // It needs the depth prepass, which nothing else uses: both together.
        if fills && !culled {
            commands.entity(entity).insert((DepthPrepass, OcclusionCulling));
        } else if !fills && culled {
            if ssao {
                commands.entity(entity).remove::<OcclusionCulling>();
            } else {
                commands.entity(entity).remove::<(DepthPrepass, OcclusionCulling)>();
            }
        }
        let clear_color = if has_sky { ClearColorConfig::None } else { ClearColorConfig::Default };
        if std::mem::discriminant(&camera.clear_color) != std::mem::discriminant(&clear_color) {
            camera.clear_color = clear_color;
        }
        rotation = scene_view.rotation;
        let placed = Transform::from_translation(scene_view.position).with_rotation(rotation);
        if *transform != placed {
            *transform = placed;
        }
    }
    for (mut camera, mut transform, sky, mut projection) in &mut skies {
        view.set_fov(&mut projection, view_fov);
        set_camera(&mut camera, viewport.is_some() && sky.position.is_some(), viewport.as_ref());
        // The sky's world turns by the inverse of the `SkyZoneInfo`'s rotation before the view's, so its
        // camera looks along the zone's rotation applied to the view's.
        let (position, turn) = sky.position.unwrap_or((Vec3::ZERO, Quat::IDENTITY));
        let placed = Transform::from_translation(position).with_rotation(turn * rotation);
        if *transform != placed {
            *transform = placed;
        }
    }
}

#[derive(Resource, Default)]
struct AnimatedMaterials {
    textures: Vec<MovingUv>,
    speculars: Vec<MovingUv>,
}

struct MovingUv {
    material: SectionMaterial,
    modifiers: Vec<UvModifier>,
    size: [f32; 2],
}

pub(crate) fn set_uv_transform<M: bevy::asset::Asset>(
    store: &mut Assets<M>,
    handle: &Handle<M>,
    transform: Affine2,
    base: impl Fn(&M) -> &StandardMaterial,
    base_mut: impl Fn(&mut M) -> &mut StandardMaterial,
) {
    if store.get(handle).is_some_and(|material| base(material).uv_transform != transform)
        && let Some(mut material) = store.get_mut(handle)
    {
        base_mut(&mut material).uv_transform = transform;
    }
}

fn animate_materials(
    time: Res<Time>,
    animated: Res<AnimatedMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut lightmapped: ResMut<Assets<LightmappedMaterial>>,
    mut mesh_lit: ResMut<Assets<MeshLitMaterial>>,
    mut blended: ResMut<Assets<BlendedMaterial>>,
    mut terrain_layers: ResMut<Assets<TerrainLayerMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for MovingUv { material, modifiers, size } in &animated.textures {
        let [[a, b, c], [d, e, f]] = uv_matrix(modifiers, seconds, *size);
        let transform = Affine2::from_cols(Vec2::new(a, d), Vec2::new(b, e), Vec2::new(c, f));
        match material {
            SectionMaterial::Standard(handle) => set_uv_transform(&mut materials, handle, transform, |m| m, |m| m),
            SectionMaterial::Lightmapped(handle) => set_uv_transform(&mut lightmapped, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::MeshLit(handle) => set_uv_transform(&mut mesh_lit, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::Blended(handle) => set_uv_transform(&mut blended, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::Terrain(_) | SectionMaterial::TerrainPass(_) => {}
        }
    }
    for MovingUv { material, modifiers, size } in &animated.speculars {
        let [[a, b, c], [d, e, f]] = uv_matrix(modifiers, seconds, *size);
        let (u, v) = (Vec4::new(a, b, c, 0.0), Vec4::new(d, e, f, 0.0));
        match material {
            SectionMaterial::Lightmapped(handle) => lightmapped.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::MeshLit(handle) => mesh_lit.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::Blended(handle) => blended.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::Terrain(handle) => terrain_layers.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::Standard(_) | SectionMaterial::TerrainPass(_) => None,
        };
    }
}

pub struct SwitchedLook {
    pub was: SectionView,
    pub now: SectionView,
    pub size: [f32; 2],
    pub specular_size: [f32; 2],
}

fn surface_alpha_mode(drawn: AlphaMode, was: AlphaMode, now: AlphaMode) -> AlphaMode {
    let plain = |mode: AlphaMode| if mode == AlphaMode::Premultiplied { AlphaMode::Blend } else { mode };
    if plain(drawn) == plain(was) { now } else { drawn }
}

#[expect(clippy::fn_params_excessive_bools, reason = "three states of one surface side flag")]
fn surface_two_sided(drawn: bool, was: bool, now: bool) -> bool {
    now || (drawn && !was)
}

pub fn restyle_switched(world: &mut bevy::ecs::world::World, looks: &HashMap<bevy::asset::AssetId<Image>, SwitchedLook>) {
    let mut restyled = restyle::<StandardMaterial>(world, looks, |m| m, |m| m, |_, _| {}, SectionMaterial::Standard);
    restyled.extend(restyle::<LightmappedMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), BakedLight::new(layers)),
        SectionMaterial::Lightmapped,
    ));
    restyled.extend(restyle::<MeshLitMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), DynamicLit::new(layers)),
        SectionMaterial::MeshLit,
    ));
    restyled.extend(restyle::<BlendedMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), OpaqueDecals::new(layers)),
        SectionMaterial::Blended,
    ));
    let Some(mut animated) = world.get_resource_mut::<AnimatedMaterials>() else { return };
    for (material, image) in restyled {
        let Some(SwitchedLook { now, size, specular_size, .. }) = looks.get(&image) else { continue };
        animated.textures.retain(|moving| moving.material != material);
        animated.speculars.retain(|moving| moving.material != material);
        if !now.uv.is_empty() {
            animated.textures.push(MovingUv { material: material.clone(), modifiers: now.uv.clone(), size: *size });
        }
        if now.layers.flat_specular.is_some() && !now.layers.specular_uv.is_empty() {
            animated.speculars.push(MovingUv { material, modifiers: now.layers.specular_uv.clone(), size: *specular_size });
        }
    }
}

fn restyle<M: bevy::asset::Asset>(
    world: &mut bevy::ecs::world::World,
    looks: &HashMap<bevy::asset::AssetId<Image>, SwitchedLook>,
    base: impl Fn(&M) -> &StandardMaterial,
    base_mut: impl Fn(&mut M) -> &mut StandardMaterial,
    layered: impl Fn(&mut M, &Layers),
    wrap: impl Fn(Handle<M>) -> SectionMaterial,
) -> Vec<(SectionMaterial, bevy::asset::AssetId<Image>)> {
    let Some(mut store) = world.get_resource_mut::<Assets<M>>() else { return Vec::new() };
    let drawing: Vec<_> = store
        .iter()
        .filter_map(|(id, material)| {
            let image = base(material).base_color_texture.as_ref()?.id();
            looks.contains_key(&image).then_some((id, image))
        })
        .collect();
    let mut restyled = Vec::new();
    for (id, image) in drawing {
        let (Some(look), Some(handle)) = (looks.get(&image), store.get_strong_handle(id)) else { continue };
        let Some(mut material) = store.get_mut(id) else { continue };
        let material = &mut *material;
        let drawn = base_mut(material);
        drawn.alpha_mode = surface_alpha_mode(drawn.alpha_mode, look.was.alpha_mode, look.now.alpha_mode);
        drawn.double_sided = surface_two_sided(drawn.double_sided, look.was.two_sided, look.now.two_sided);
        drawn.cull_mode = if drawn.double_sided { None } else { Some(Face::Back) };
        let [r, g, b, a] = look.now.tint;
        drawn.base_color = Color::linear_rgba(r, g, b, a);
        drawn.emissive = look.now.emissive();
        drawn.emissive_texture.clone_from(&look.now.glow);
        drawn.uv_transform = Affine2::IDENTITY;
        layered(material, &look.now.layers);
        restyled.push((wrap(handle), image));
    }
    restyled
}

fn zone_fog(
    time: Res<Time>,
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    mut cameras: Query<(&SceneView, &mut DistanceFog)>,
) {
    let model = collision.model();
    let t = 1.0 - (-time.delta_secs() / FOG_BLEND_SECONDS).exp();
    for (view, mut fog) in &mut cameras {
        let zone = model.and_then(|model| {
            let position = from_bevy(view.position - shown.origin);
            shown.fogs.get(usize::from(p2_collision::point_zone(model, position.into()))).copied().flatten()
        });
        let target = zone.map(|zone| {
            let [r, g, b] = zone.color;
            (Color::srgb_u8(r, g, b).to_linear(), zone.start, zone.end.max(zone.start + 1.0))
        });
        let (start, end) = match fog.falloff {
            FogFalloff::Linear { start, end } => (start, end),
            _ => (0.0, 1.0),
        };
        let shown = (fog.color.to_linear(), start, end);
        let blended = blend_fog(shown, target, t);
        if blended != shown {
            let (color, start, end) = blended;
            fog.color = color.into();
            fog.falloff = FogFalloff::Linear { start, end };
        }
    }
}

/// Retail snaps zone fog at a zone crossing; ours fades in both looks.
const FOG_BLEND_SECONDS: f32 = 0.35;

/// `target` `None` = no fog: the colour fades out where it is. Fog coming from none starts at the target's colour and
/// distances, fully transparent, so only its strength fades in; within 1/512 it lands on the target (no fog lands on
/// [`LinearRgba::NONE`], as retail shows it).
fn blend_fog(shown: (LinearRgba, f32, f32), target: Option<(LinearRgba, f32, f32)>, t: f32) -> (LinearRgba, f32, f32) {
    let (color, start, end) = shown;
    let landed = target.map_or(LinearRgba::NONE, |(to, _, _)| to);
    let (to, to_start, to_end) = target.unwrap_or((color.with_alpha(0.0), start, end));
    let (color, start, end) = if color.alpha <= 0.0 { (to.with_alpha(0.0), to_start, to_end) } else { (color, start, end) };
    let mix = |a: f32, b: f32| a + (b - a) * t;
    let blended = LinearRgba::new(mix(color.red, to.red), mix(color.green, to.green), mix(color.blue, to.blue), mix(color.alpha, to.alpha));
    let near = [blended.red - to.red, blended.green - to.green, blended.blue - to.blue, blended.alpha - to.alpha].iter().all(|d| d.abs() < 1.0 / 512.0);
    let (start, end) = (mix(start, to_start), mix(end, to_end));
    if near && (start - to_start).abs() < 1.0 && (end - to_end).abs() < 1.0 { (landed, to_start, to_end) } else { (blended, start, end) }
}

pub(crate) fn merged_box((min, max): ([f32; 3], [f32; 3]), (low, high): ([f32; 3], [f32; 3])) -> ([f32; 3], [f32; 3]) {
    ([0, 1, 2].map(|k| min[k].min(low[k])), [0, 1, 2].map(|k| max[k].max(high[k])))
}

pub(crate) fn bounds(points: impl IntoIterator<Item = [f32; 3]>) -> Option<([f32; 3], [f32; 3])> {
    points.into_iter().fold(None, |bounds, p| Some(merged_box(bounds.unwrap_or((p, p)), (p, p))))
}

fn follower_zones(
    mut commands: Commands,
    collision: Res<LevelCollision>,
    moved: Query<&ChildOf, (With<FollowActor>, Or<(Changed<Transform>, Changed<Aabb>)>)>,
    mut roots: Query<(&Children, Option<&mut ZoneCulled>), With<ActorModel>>,
    sections: Query<(&Transform, &Aabb, Has<crate::map::SpriteQuad>), With<FollowActor>>,
) {
    let model = collision.model();
    let Some(model) = model.filter(|model| !model.leaves.is_empty()) else { return };
    let dirty: HashSet<Entity> = moved.iter().map(ChildOf::parent).collect();
    for root in dirty {
        let Ok((children, culled)) = roots.get_mut(root) else { continue };
        let corners = children.iter().filter_map(|&child| sections.get(child).ok()).flat_map(|(transform, aabb, sprite)| {
            let (center, half) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
            let any_facing = (half * transform.scale).length();
            (0..8).map(move |i| {
                let sign = Vec3::new([-1.0, 1.0][i & 1], [-1.0, 1.0][(i >> 1) & 1], [-1.0, 1.0][i >> 2]);
                from_bevy(if sprite { transform.translation + sign * any_facing } else { transform.transform_point(center + sign * half) })
            })
        });
        let Some((min, max)) = bounds(corners) else { continue };
        if culled.as_ref().is_some_and(|zone| zone.min == min && zone.max == max) {
            continue;
        }
        let zone = ZoneCulled { zones: p2_collision::box_zones(model, min.into(), max.into()), min, max, only_from: None };
        match culled {
            Some(mut culled) => *culled = zone,
            None => {
                commands.entity(root).insert(zone);
            }
        }
    }
}

// No near plane: it would clip away a portal closer to the eye than that plane, and lose the zone behind.
fn walk_view(transform: &Transform, projection: &Projection, origin: Vec3) -> Vec<Plane> {
    let frustum = projection.compute_frustum(&GlobalTransform::from(*transform));
    let sides = frustum.0.half_spaces.iter().enumerate().filter(|&(index, _)| index != bevy::shape::ViewFrustum::NEAR_PLANE_IDX);
    sides
        .map(|(_, half)| {
            // Bevy half-spaces (inside: n · p + d > 0) moved to Unreal space (p = swap(u) + origin).
            let normal = Vec3::from(half.normal());
            let [x, y, z] = from_bevy(normal);
            Plane::from([x, y, z, -(half.d() + normal.dot(origin))])
        })
        .collect()
}

/// Scenes without BSP portals, and a view outside the world, show everything. While the modern sun casts ([`SunShadow`]),
/// a dynamic object with a sun caster mesh ([`CASTER_LAYER`]) within the sun's last cascade stays shown:
/// `Visibility::Hidden` takes a mesh out of the shadow pass too, so its shadow popped in and out with the view.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn zone_visibility(
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    cameras: Query<(&Transform, &Projection), With<SceneView>>,
    suns: Query<&CascadeShadowConfig, With<SunShadow>>,
    layers: Query<&RenderLayers>,
    mut culled: Query<(Entity, &ZoneCulled, &mut Visibility, Option<&Children>)>,
    mut shadow_only: ResMut<ShadowOnly>,
    mut world_seen: ResMut<WorldSeenZones>,
) {
    shadow_only.0.clear();
    let model = collision.model();
    let view_zone = model.as_ref().zip(cameras.iter().next()).map(|(model, (transform, _))| {
        p2_collision::point_zone(model, from_bevy(transform.translation - shown.origin).into())
    });
    let mut eye_seen = None;
    let seen = match (&shown.portals, cameras.iter().next(), view_zone) {
        (Some(portals), Some((transform, projection)), Some(zone)) => {
            let eye = from_bevy(transform.translation - shown.origin);
            let view = walk_view(transform, projection, shown.origin);
            eye_seen = Some(eye);
            portals.visible(eye.into(), zone, &view)
        }
        _ => VisibleZones::all(),
    };
    if seen.capped() {
        bevy::log::warn_once!("portal walk passed its cap: no zone culling at such views");
    }
    let mut seen_bits = [0u32; 8];
    for zone in 0..=u8::MAX {
        if seen.reached(zone) {
            seen_bits[usize::from(zone >> 5)] |= 1 << (zone & 31);
        }
    }
    world_seen.set_if_neq(WorldSeenZones(seen_bits));
    let sun = eye_seen.zip(suns.iter().next().and_then(|config| config.bounds.last().copied()));
    let caster = RenderLayers::layer(CASTER_LAYER);
    for (root, zone, mut visibility, children) in &mut culled {
        let forced = forced_shown(zone.only_from.as_deref(), view_zone);
        let seen = forced && zone.zones.iter().any(|&z| seen.sees(z, zone.min.into(), zone.max.into()));
        let drawn = seen
            || (forced
                && sun.is_some_and(|(eye, reach)| sun_reaches(eye, reach, zone))
                && children.is_some_and(|children| children.iter().any(|&child| layers.get(child).is_ok_and(|l| l.intersects(&caster)))));
        if drawn && !seen {
            shadow_only.0.insert(root);
        }
        visibility.set_if_neq(if drawn { Visibility::Inherited } else { Visibility::Hidden });
    }
}

fn forced_shown(only_from: Option<&[u8]>, view_zone: Option<u8>) -> bool {
    only_from.is_none_or(|zones| view_zone.is_none_or(|view| zones.contains(&view)))
}

fn sun_reaches(eye: [f32; 3], reach: f32, zone: &ZoneCulled) -> bool {
    let gap = |k: usize| eye[k].max(zone.min[k]).min(zone.max[k]) - eye[k];
    gap(0).powi(2) + gap(1).powi(2) + gap(2).powi(2) <= reach * reach
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::EulerRot;
    use p2_assets::Model;

    fn drawn_as(state: RenderState) -> SectionView {
        SectionView::textured(Handle::default(), Some(MaterialImages { image: Handle::default(), glow: None, detail: None, specular: None, state }))
    }

    #[test]
    fn switched_material_takes_scrolling_blending_and_sides() {
        let mut world = bevy::ecs::world::World::new();
        let image: Handle<Image> = Handle::default();
        let mut store = Assets::<StandardMaterial>::default();
        let material = store.add(StandardMaterial { base_color_texture: Some(image.clone()), ..Default::default() });
        world.insert_resource(store);
        world.init_resource::<AnimatedMaterials>();
        let still = RenderState::default();
        let panned = RenderState {
            blending: Some(Blending::Additive),
            two_sided: true,
            uv: vec![UvModifier::Panner { direction: [1.0, 0.0], rate: 1.0 }],
            ..Default::default()
        };
        let switch = |world: &mut bevy::ecs::world::World, was: &RenderState, now: &RenderState| {
            let look = SwitchedLook { was: drawn_as(was.clone()), now: drawn_as(now.clone()), size: [1.0; 2], specular_size: [1.0; 2] };
            restyle_switched(world, &HashMap::from([(image.id(), look)]));
        };

        switch(&mut world, &still, &panned);
        let drawn = world.resource::<Assets<StandardMaterial>>().get(&material).cloned().unwrap();
        assert_eq!(drawn.alpha_mode, AlphaMode::Add);
        assert!(drawn.double_sided);
        assert_eq!(drawn.cull_mode, None);
        assert_eq!(world.resource::<AnimatedMaterials>().textures.len(), 1);

        switch(&mut world, &panned, &panned);
        assert_eq!(world.resource::<AnimatedMaterials>().textures.len(), 1);
        switch(&mut world, &panned, &still);
        let drawn = world.resource::<Assets<StandardMaterial>>().get(&material).cloned().unwrap();
        assert_eq!(drawn.alpha_mode, AlphaMode::Opaque);
        assert_eq!(drawn.cull_mode, Some(Face::Back));
        assert_eq!(drawn.uv_transform, Affine2::IDENTITY);
        assert!(world.resource::<AnimatedMaterials>().textures.is_empty());
    }

    #[test]
    fn a_surfaces_own_flags_outlast_a_switch() {
        assert_eq!(surface_alpha_mode(AlphaMode::Add, AlphaMode::Opaque, AlphaMode::Blend), AlphaMode::Add);
        assert!(surface_two_sided(true, false, false));
        assert_eq!(surface_alpha_mode(AlphaMode::Opaque, AlphaMode::Opaque, AlphaMode::Add), AlphaMode::Add);
        assert_eq!(surface_alpha_mode(AlphaMode::Premultiplied, AlphaMode::Blend, AlphaMode::Opaque), AlphaMode::Opaque);
        assert!(!surface_two_sided(true, true, false));
        assert!(surface_two_sided(false, false, true));
    }

    #[test]
    fn sun_reaches_by_the_box_nearest_point() {
        let zone = ZoneCulled { zones: vec![1], min: [100.0, -10.0, -10.0], max: [120.0, 10.0, 10.0], only_from: None };
        assert!(sun_reaches([0.0; 3], 100.0, &zone));
        assert!(!sun_reaches([0.0; 3], 99.0, &zone));
        assert!(sun_reaches([110.0, 0.0, 0.0], 0.0, &zone));
    }

    #[test]
    fn fog_snaps_at_full_step_and_blends_in_steps() {
        let red = (LinearRgba::new(1.0, 0.0, 0.0, 1.0), 100.0, 900.0);
        let blue = (LinearRgba::new(0.0, 0.0, 1.0, 1.0), 300.0, 2000.0);
        let none = (LinearRgba::NONE, 0.0, 100.0);
        assert_eq!(blend_fog(red, Some(blue), 1.0), blue);
        assert_eq!(blend_fog(red, None, 1.0), (LinearRgba::NONE, 100.0, 900.0));
        assert_eq!(blend_fog(none, Some(red), 1.0), red);
        let (color, start, end) = blend_fog(red, Some(blue), 0.5);
        assert_eq!((color.red, color.blue, start, end), (0.5, 0.5, 200.0, 1450.0));
        let (color, start, end) = blend_fog(none, Some(red), 0.25);
        assert_eq!((color.red, color.alpha, start, end), (1.0, 0.25, 100.0, 900.0));
        let (color, start, _) = blend_fog(red, None, 0.25);
        assert_eq!((color.red, color.alpha, start), (1.0, 0.75, 100.0));
        let mut shown = red;
        for _ in 0..100 {
            shown = blend_fog(shown, None, 0.2);
        }
        assert_eq!(shown.0, LinearRgba::NONE);
    }

    #[test]
    fn view_rotation_matches_yaw_pitch_and_rolls_about_the_view() {
        let unit = std::f32::consts::TAU / 65536.0;
        for [pitch, yaw] in [[0, 0], [0, 16384], [3000, -12000], [-8000, 40000], [62000, 5000]] {
            let rig = Quat::from_euler(EulerRot::YXZ, -(yaw as f32) * unit - std::f32::consts::FRAC_PI_2, (pitch as i16) as f32 * unit, 0.0);
            let view = view_rotation([pitch, yaw, 0].into());
            assert!(rig.dot(view).abs() > 0.9999, "pitch {pitch} yaw {yaw}: {rig} vs {view}");
        }
        let (level, rolled) = (view_rotation([2000, 9000, 0].into()), view_rotation([2000, 9000, 8192].into()));
        assert!((level * -Vec3::Z).distance(rolled * -Vec3::Z) < 1e-4, "roll keeps the view direction");
        assert!((level * Vec3::Y).dot(rolled * Vec3::Y) < 0.75, "roll tilts up");
    }

    fn two_zones() -> Model {
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
        Model {
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

    #[test]
    fn the_walk_keeps_a_portal_nearer_than_the_near_plane() {
        let origin = Vec3::new(100.0, 200.0, 300.0);
        let transform = Transform::from_translation(origin);
        let projection = Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 500_000.0, ..Default::default() });
        let view = walk_view(&transform, &projection, origin);
        assert_eq!(view.len(), 5);
        let ahead = from_bevy(Vec3::new(0.0, 0.0, -1.0));
        assert!(view.iter().all(|plane| plane.normal.dot(ahead.into()) >= plane.d));
    }

    #[test]
    fn a_forced_zone_tag_shows_only_from_its_zones() {
        assert!(forced_shown(None, Some(3)));
        assert!(forced_shown(Some(&[1, 3]), Some(3)));
        assert!(!forced_shown(Some(&[1, 3]), Some(2)));
        assert!(!forced_shown(Some(&[]), Some(0)));
        assert!(forced_shown(Some(&[1]), None));
    }

    #[test]
    fn a_moving_actor_is_in_the_zones_where_it_is() {
        let mut app = App::new();
        let collision = StaticCollision { model: two_zones(), terrains: Vec::new(), meshes: Vec::new() };
        app.insert_resource(LevelCollision(Some(Arc::new(collision))));
        app.add_systems(Update, follower_zones);
        let follow = FollowActor { actor: ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
        let root = app.world_mut().spawn(ActorModel(follow.actor)).id();
        let aabb = Aabb::from_min_max(Vec3::splat(-10.0), Vec3::splat(10.0));
        let section = app.world_mut().spawn((follow, Transform::from_xyz(0.0, 50.0, 0.0), aabb, ChildOf(root))).id();
        let zones = |app: &App| app.world().get::<ZoneCulled>(root).map(|zone| zone.zones.clone());
        app.update();
        assert_eq!(zones(&app), Some(vec![2]));
        for (y, expected) in [(5.0, vec![1, 2]), (-50.0, vec![1])] {
            if let Some(mut transform) = app.world_mut().get_mut::<Transform>(section) {
                transform.translation.y = y;
            }
            app.update();
            assert_eq!(zones(&app), Some(expected));
        }
    }

}
