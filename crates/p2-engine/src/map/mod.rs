use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers, Visibility};
use bevy::ecs::prelude::{AnyOf, ChildOf, Commands, Entity, Has, Query, Res, ResMut, With, Without};
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::material::AlphaMode;
use bevy::math::{Mat4, UVec2, Vec2, Vec3, Vec4};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Indices, Mesh, Mesh3d, MeshTag, PrimitiveTopology};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::render::render_resource::Face;
use bevy::transform::components::Transform;
use bevy::camera::{Camera, Camera2d, CameraOutputMode, ClearColorConfig, Projection};
use bevy::color::Color;
use bevy_egui::{EguiContexts, EguiGlobalSettings, EguiTextureHandle, PrimaryEguiContext, egui};
use rustc_hash::{FxHashMap, FxHashSet};
use wgpu_types::BlendState;
use p2_assets::{Blending, DecoMaps, Font, MeshAnimation, Model, ModelPolygon, RenderState, SkeletalMesh, StaticMesh, TexelMap, poly_flags, read_convex_volume, read_font, read_level, read_mesh_animation, read_model, read_skeletal_mesh, read_static_mesh, read_static_mesh_instance, read_static_mesh_materials, read_terrain, texture_alpha, texture_bgra, texture_heights};
use p2_render_cpu::{AntiportalActor, MeshPlacement, instance_fits};
use p2_bevy_assets::{
    alpha_map_image, bone_matrices, lighting_colors, model_polygons_to_mesh, modulate_2x, rotator_matrix, skeletal_mesh_to_meshes, skin_bind, static_mesh_to_meshes,
    terrain_layer_mesh, texture_to_image, to_bevy, unreal_mesh_matrix, unreal_to_bevy, unreal_transform,
};
use p2_collision::MeshCollision;
use p2_package::{Ini, LoadedPackage, Loader, Package, PackageId};
use p2_particles::{DrawStyle, Emitter, SpriteParticle};
use p2_vm::{CanvasItem, Kind, ObjectId, ParticleCommand, ProjectorEvent, Vm};

use crate::render::actor_light::{
    ActorLighting, ActorLit, DEFAULT_MAX_LIGHTS, LightEnvironment, MAX_HARDWARE_LIGHTS, MeshLighting, Shading, keep_for_relighting,
};
use crate::actors::{
    EmitterActor, MeshActor, ProjectorActor, SkeletalActor, SpriteActor, antiportal_actor, emitter_actor, location_rotation, mesh_actor, mesh_light,
    projector_actor, skeletal_actor, sprite_actor, zone_ambient, zone_fog,
};
use crate::animation::{Animated, AnimatedMesh, FollowBone};
use crate::library::{Game, Library, LoadError, LoadProgress, PlayLoaderTask};
use crate::loading::ProgressBar;
use crate::mesh_view::{
    ActorModel, CanvasDrawn, FollowActor, MaterialImages, MeshView, ModelView, MapModel, MovingModel, Scene, SectionMaterial, SectionView, Sky, SceneView, Skin,
    ZoneCulled, OVERLAY_LAYERS, bounds, merged_box, spawn_actor_sections,
};
use crate::render::mesh_light::MeshLitMaterial;
use crate::render::particle_material::ParticleMaterial;
use crate::play::{FrameTimes, Overlay, OverlayTitle, Play, PlayWorld};
pub use crate::level_collision::{LevelCollision, StaticCollision};

mod coronas;
mod fluids;
mod hud;
mod movie;
mod particles;
mod scene;
mod spawned;
mod sprites;

use hud::{HudAssets, HudFont, paint_canvas, paint_hud, paint_overlay};
use movie::Movies;
use particles::ParticleActor;
pub use particles::ParticleReport;
pub use particles::{emitter_sort_bias, pad_bounds};
pub use sprites::{SpriteQuad, face_sprites};
pub use coronas::{CoronaActor, corona_actor, draw_coronas, is_corona};
pub use spawned::material_images;
pub use scene::{SceneAssets, SectionChunks, animate_mesh_with, sections_of_static_mesh, skeletal_mesh_sections, skeletal_sections};
use scene::{ActorTexture, projector_image, projector_tag, projector_transform, scene_preparer};
pub use scene::projector_box;

pub fn spawn_hud_camera(mut commands: Commands, mut egui_settings: ResMut<EguiGlobalSettings>) {
    egui_settings.auto_create_primary_context = false;
    commands.spawn((
        PrimaryEguiContext,
        Camera2d,
        RenderLayers::none(),
        Camera {
            // After the 3D view (0), the HUD's actors and canvas layers (1 to 8) and the gamma ramp (9).
            order: 10,
            // Additive canvas tiles have colour but no alpha.
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..Default::default()
        },
    ));
}

#[derive(SystemParam)]
pub struct SceneTargets<'w, 's> {
    pub assets: AssetStores<'w>,
    pub(crate) particle_view: ParticleView<'w, 's>,
    pub(crate) canvas: CanvasModels<'w, 's>,
    pub(crate) models: ActorModels<'w, 's>,
    pub view: ResMut<'w, MeshView>,
    pub(crate) shown: ResMut<'w, crate::shown_scene::ShownScene>,
    pub animated: ResMut<'w, Animated>,
    pub times: ResMut<'w, FrameTimes>,
    pub play: ResMut<'w, Play>,
    pub commands: Commands<'w, 's>,
}

#[derive(SystemParam)]
pub struct AssetStores<'w> {
    pub images: ResMut<'w, Assets<Image>>,
    pub meshes: ResMut<'w, Assets<Mesh>>,
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub mesh_lit: ResMut<'w, Assets<MeshLitMaterial>>,
    pub particle_materials: ResMut<'w, Assets<ParticleMaterial>>,
    pub sprite_buffers: Option<ResMut<'w, Assets<bevy::render::storage::ShaderBuffer>>>,
    pub audio: Option<ResMut<'w, Assets<bevy::audio::AudioSource>>>,
    pub bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
    pub wet: ResMut<'w, crate::render::wet::WetTextures>,
}

#[derive(SystemParam)]
pub(crate) struct ParticleView<'w, 's> {
    pub(crate) collision: Res<'w, LevelCollision>,
    pub(crate) cameras: Query<'w, 's, &'static Transform, With<SceneView>>,
    pub(crate) projections: Query<'w, 's, &'static Projection, With<SceneView>>,
    pub(crate) zone_culled: Query<'w, 's, &'static Visibility, With<ZoneCulled>>,
    pub(crate) places: Option<Res<'w, crate::animation::AttachmentPlaces>>,
    pub(crate) pool_slots: Query<'w, 's, &'static mut Transform, (With<particles::PoolSlot>, Without<FollowActor>, Without<SceneView>)>,
}

impl ParticleView<'_, '_> {
    pub(crate) fn frustum_test(&self, origin: Vec3) -> impl Fn(Option<(Vec3, Vec3)>) -> bool + use<> {
        let camera = self.cameras.iter().next();
        let frustum = camera.zip(self.projections.iter().next()).map(|(camera, projection)| projection.compute_frustum(&bevy::transform::components::GlobalTransform::from(*camera)));
        let at_origin = bevy::math::Affine3A::from_translation(origin);
        move |bounds| match (bounds, &frustum) {
            (Some((min, max)), Some(frustum)) => frustum.intersects_obb(&bevy::camera::primitives::Aabb::from_min_max(min, max), &at_origin, true, true),
            _ => true,
        }
    }
}

#[derive(SystemParam)]
pub(crate) struct CanvasModels<'w, 's> {
    pub(crate) canvas_models: Query<'w, 's, (&'static FollowActor, &'static CanvasDrawn, &'static mut Transform), Without<SceneView>>,
    pub(crate) canvas_sections: Query<'w, 's, (Entity, &'static ChildOf), With<CanvasDrawn>>,
    pub(crate) attached: Query<
        'w,
        's,
        (Entity, &'static FollowActor, &'static mut Transform, &'static mut Visibility, Option<&'static RenderLayers>),
        (Without<CanvasDrawn>, Without<SceneView>, Without<ZoneCulled>),
    >,
}

#[derive(SystemParam)]
pub(crate) struct ActorModels<'w, 's> {
    pub(crate) followers: Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>, Option<&'static ChildOf>), Without<CanvasDrawn>>,
    pub(crate) untagged: Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>), (Without<CanvasDrawn>, Without<MeshTag>)>,
    pub(crate) actor_models: Query<'w, 's, (Entity, &'static ActorModel, Has<MapModel>)>,
}

struct CanvasActor {
    actor: ObjectId,
    call: usize,
    entity: Entity,
    slot: Option<usize>,
}

pub struct Session {
    root: PathBuf,
    pub library: Library,
    map: Option<String>,
    map_system: Option<PathBuf>,
    hud_textures: FxHashMap<String, Option<(egui::TextureId, egui::Vec2)>>,
    hud_fonts: FxHashMap<String, Option<HudFont>>,
    /// Keeps those images alive: egui holds them weakly and forgets each once it goes.
    hud_images: Vec<Handle<Image>>,
    movies: Movies,
    hud_assets: HudAssets,
    canvas_actors: Vec<CanvasActor>,
    failed_canvas_meshes: HashSet<String>,
    /// Kept drawn after a load until the new map's first frame replaces the load screen, as the engine's frame buffer keeps
    /// them.
    left_canvas_actors: Vec<Entity>,
    dynamic_decals: FxHashMap<ObjectId, Entity>,
    projector_actors: FxHashSet<ObjectId>,
    decal_images: HashMap<String, Option<(Handle<Image>, Vec2, Option<p2_assets::FinalBlend>)>>,
    switch_images: HashMap<(PackageId, usize), Vec<scene::SwitchImage>>,
    switch_targets: FxHashMap<(PackageId, usize), Option<scene::SwitchTarget>>,
    spawn_models: HashMap<spawned::ModelKey, Option<spawned::SpawnModel>>,
    spawn_materials: HashMap<crate::mesh_view::ActorMaterialKey, SectionMaterial>,
    spawn_builds: Vec<spawned::ModelBuild>,
    texture_builds: Vec<spawned::TextureBuild>,
    waiting_sprites: Vec<SpriteActor>,
    waiting_coronas: Vec<CoronaActor>,
    spawn_waiting: Vec<spawned::SpawnedActor>,
    particles: Vec<ParticleActor>,
    particle_emitters: FxHashMap<ObjectId, (usize, usize)>,
    actor_textures: HashMap<String, Option<ActorTexture>>,
    pool_materials: HashMap<particles::PoolMaterialKey, Handle<StandardMaterial>>,
    particle_records: Vec<[u32; crate::render::particle_material::RECORD_WORDS]>,
    corona_materials: HashMap<String, Option<Handle<ParticleMaterial>>>,
    fluids: Vec<fluids::FluidShown>,
    pending: Option<PendingLoad>,
    /// Loading frames show it again: the engine does not clear the screen.
    last_hud: Arc<[CanvasItem]>,
    animations: HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    vm_animations: scene::VmAnimations,
    progress_bar: Option<Option<(ProgressBar, egui::TextureId)>>,
}

pub fn map_package(name: &str) -> String {
    let stem = name.len().checked_sub(4).filter(|&cut| name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".fuk")).map_or(name, |cut| &name[..cut]);
    Path::new("Maps").join(format!("{stem}.fuk")).display().to_string()
}

pub fn start_map(args: impl IntoIterator<Item = String>, root: &Path) -> Option<String> {
    args.into_iter().find(|arg| {
        let path = Path::new(arg);
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) || path.is_file() || arg.starts_with(['-', '+']) || arg.contains('=') {
            return false;
        }
        root.join(map_package(arg)).is_file()
    })
}

pub fn travel_package(url: &str) -> Option<String> {
    if let Some(map) = crate::saves::load_slot(url).and_then(crate::saves::saved_map) {
        return Some(map_package(&map));
    }
    let map = p2_vm::TravelUrl::parse(url).map;
    let name = map.trim();
    (!name.is_empty()).then(|| map_package(name))
}

pub trait SessionHolder: bevy::ecs::resource::Resource<Mutability = bevy::ecs::component::Mutable> {
    fn session(&mut self) -> &mut Session;
}

pub fn present_session<R: SessionHolder>(holder: Option<ResMut<R>>, mut t: SceneTargets) {
    let Some(mut holder) = holder else { return };
    let session = holder.session();
    if session.loading().is_some() {
        return;
    }
    let Some(world) = t.play.world() else { return };
    let presented = world.presented();
    let items = if presented { world.hud_items() } else { Arc::default() };
    if presented {
        session.drop_left_canvas_actors(&mut t);
    }
    session.present(&items, &mut t);
}

/// Frames the engine draws as travel begins (P2Screen.uc: three duplicates, one for a loading message, and one with
/// the screen drawn under the progress bar).
const TRAVEL_FRAMES: usize = 5;

struct PendingLoad {
    package: String,
    progress: Arc<LoadProgress>,
    task: PlayLoaderTask<scene::PreparedScene>,
}

pub(crate) struct StartedVm {
    vm: Vm<Arc<Loader>>,
    loader: Arc<Loader>,
    id: PackageId,
    script_errors: usize,
    player: Option<(ObjectId, Option<ObjectId>)>,
}

fn start_vm(
    previous: Option<Vm<Arc<Loader>>>,
    url: Option<&str>,
    loader: Arc<Loader>,
    id: PackageId,
    configs: Vec<(&'static str, Ini)>,
    progress: &LoadProgress,
) -> StartedVm {
    progress.set(0.5);
    let fresh = previous.is_none();
    let mut vm = match previous {
        Some(mut vm) => {
            if let Err(error) = vm.leave_level() {
                bevy::log::warn!("travel: {error}");
            }
            vm.set_loader(Arc::clone(&loader));
            vm
        }
        None => {
            let mut vm = Vm::new(Arc::clone(&loader));
            for (name, ini) in configs {
                vm.add_config(name, ini);
            }
            if vm.redirect_class(("Shell", "MenuOptions"), ("P2Modern", "ModernMenuOptions")) {
                bevy::log::info!("modern menu: P2Modern.ModernMenuOptions found, Options page swapped");
            } else {
                bevy::log::info!("modern menu: P2Modern.ModernMenuOptions not found, retail Options");
            }
            vm
        }
    };
    if fresh && let Some(dir) = crate::saves::save_dir() {
        vm.options.data_dir = dir.parent().map(Path::to_path_buf);
        vm.set_save_dir(dir);
    }
    let options = url.and_then(|url| url.find(['?', '#']).map(|i| &url[i..])).unwrap_or_default();
    let save = url.and_then(crate::saves::load_slot).and_then(|slot| {
        let save = crate::saves::read(slot).map_err(|e| e.to_string()).and_then(|data| p2_vm::SaveGame::decode(&data).map_err(|e| e.to_string()));
        match save {
            Ok(save) if save.map.eq_ignore_ascii_case(&loader.package(id).name) => Some(save),
            Ok(save) => {
                bevy::log::warn!("load: slot {slot} is of map {}, not the one loaded", save.map);
                None
            }
            Err(error) => {
                bevy::log::warn!("load: slot {slot}: {error}");
                None
            }
        }
    });
    let script_errors = match &save {
        Some(save) => match vm.restore(id, save) {
            Ok(()) => {
                bevy::log::info!("load: {} restored from {url:?}", save.map);
                for line in vm.diagnostics.log.iter().filter(|line| line.starts_with("Load:")) {
                    bevy::log::warn!("{line}");
                }
                0
            }
            Err(error) => {
                bevy::log::warn!("load: {error}");
                1
            }
        },
        None => {
            let game_options = p2_vm::TravelUrl::parse(options).options_text();
            let game_class = match url.and_then(|url| vm.url_game_class(url)) {
                Some(class) => Ok(class),
                None => vm.find_class("GameTypes", "AWPGameInfo").or_else(|_| vm.find_class("Engine", "GameInfo")),
            };
            match game_class.map(|class| vm.begin_play(id, class, &game_options)) {
                Ok(Ok(errors)) => errors.len(),
                _ => 1,
            }
        }
    };
    progress.set(0.8);
    // The camera is the player's eyes, so its own pawn is not drawn.
    if fresh && let Err(error) = vm.init_client() {
        bevy::log::warn!("client: {error}");
    }
    let player = vm.login(options).unwrap_or_else(|error| {
        bevy::log::warn!("login: {error}");
        None
    });
    if save.is_some() {
        let path = |object: Option<ObjectId>| object.map(|o| vm.store.object(o).path.clone());
        bevy::log::info!("load: login gave controller {:?}, pawn {:?}", path(player.map(|p| p.0)), path(player.and_then(|p| p.1)));
        for (actor, error) in vm.post_load_game() {
            bevy::log::warn!("PostLoadGame {actor}: {error}");
        }
    }
    vm.prewarm();
    progress.set(0.9);
    StartedVm { vm, loader, id, script_errors, player }
}

fn open_game<'a>(library: &'a mut Library, system: Option<&Path>) -> Option<&'a mut Game> {
    library.game(system?)
}

fn bevy_vector(v: p2_math::Vector) -> Vec3 {
    Vec3::from(to_bevy(v))
}

fn export_reference(export: usize) -> i32 {
    export as i32 + 1
}

fn map_name(package: &str) -> &str {
    Path::new(package).file_stem().and_then(|s| s.to_str()).unwrap_or_default()
}

impl Session {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            library: Library::default(),
            map: None,
            map_system: None,
            hud_textures: FxHashMap::default(),
            hud_fonts: FxHashMap::default(),
            hud_images: Vec::new(),
            movies: Movies::default(),
            hud_assets: HudAssets::default(),
            canvas_actors: Vec::new(),
            failed_canvas_meshes: HashSet::new(),
            left_canvas_actors: Vec::new(),
            dynamic_decals: FxHashMap::default(),
            projector_actors: FxHashSet::default(),
            decal_images: HashMap::new(),
            switch_images: HashMap::new(),
            switch_targets: FxHashMap::default(),
            spawn_models: HashMap::new(),
            spawn_materials: HashMap::new(),
            spawn_builds: Vec::new(),
            texture_builds: Vec::new(),
            waiting_sprites: Vec::new(),
            waiting_coronas: Vec::new(),
            spawn_waiting: Vec::new(),
            particles: Vec::new(),
            particle_emitters: FxHashMap::default(),
            actor_textures: HashMap::new(),
            pool_materials: HashMap::new(),
            particle_records: Vec::new(),
            corona_materials: HashMap::new(),
            fluids: Vec::new(),
            pending: None,
            last_hud: Arc::default(),
            animations: HashMap::new(),
            vm_animations: scene::VmAnimations::default(),
            progress_bar: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn clear(&mut self, t: &mut SceneTargets) {
        t.animated.0.clear();
        t.play.set(None);
        self.map = None;
        self.map_system = None;
        self.projector_actors.clear();
        self.hud_textures.clear();
        self.hud_fonts.clear();
        self.hud_images.clear();
        self.hud_assets = HudAssets::default();
        self.movies.stop(&mut t.commands);
        for shown in self.canvas_actors.drain(..) {
            t.commands.entity(shown.entity).despawn();
        }
        self.drop_left_canvas_actors(t);
        for (_, entity) in self.dynamic_decals.drain() {
            t.commands.entity(entity).despawn();
        }
        for (entity, _, preview) in &t.models.actor_models {
            if !preview {
                t.commands.entity(entity).despawn();
            }
        }
        self.decal_images.clear();
        self.failed_canvas_meshes.clear();
        self.spawn_models.clear();
        self.spawn_materials.clear();
        self.spawn_builds.clear();
        self.texture_builds.clear();
        self.waiting_sprites.clear();
        self.waiting_coronas.clear();
        self.spawn_waiting.clear();
        for particles in self.particles.drain(..) {
            t.commands.entity(particles.parent).despawn();
        }
        self.particle_emitters.clear();
        self.actor_textures.clear();
        self.pool_materials.clear();
        self.corona_materials.clear();
        self.fluids.clear();
    }

    pub fn frame(&mut self, ctx: &egui::Context, rect: egui::Rect, contexts: &mut EguiContexts, t: &mut SceneTargets) {
        let started = std::time::Instant::now();
        t.play.set_canvas_size([rect.width(), rect.height()]);
        t.view.canvas_below = [false; OVERLAY_LAYERS.len()];
        // Until a new map's first VM frame is presented, the last image (the load screen) stays up, as in the
        // engine's frame buffer; its canvas actors belong to the level left.
        if t.play.world().is_some_and(|world| !world.presented()) {
            let items = Arc::clone(&self.last_hud);
            paint_hud(ctx, rect, &items, self, contexts, t);
            t.times.add("hud paint", started);
            return;
        }
        let items = t.play.world().map(PlayWorld::hud_items).unwrap_or_default();
        self.movies.update(&self.root, contexts, t, &mut self.hud_textures);
        if let Some(overlay) = t.play.world().and_then(PlayWorld::screen_overlay) {
            paint_overlay(ctx, rect, overlay, self, contexts, t);
        }
        if !items.is_empty() {
            paint_canvas(ctx, rect, &items, self, contexts, t);
        }
        t.times.add("hud paint", started);
        self.last_hud = items;
    }

    pub fn present(&mut self, items: &[CanvasItem], t: &mut SceneTargets) {
        // Spawned and restyled actors first: a HUD-drawn model whose `Skins` changed (the kick's foot getting bloody) is
        // made again before the HUD's models are shown, not missing for a frame.
        let started = std::time::Instant::now();
        self.update_spawned(t);
        t.times.add("spawned", started);
        let started = std::time::Instant::now();
        let calls = items.iter().filter(|item| matches!(item, CanvasItem::Actor { .. }));
        let drawn: Vec<(&CanvasItem, usize)> = calls.zip(hud::call_slots(items)).filter(|&(_, slot)| slot < OVERLAY_LAYERS.len()).collect();
        t.view.fov = t.play.world().and_then(PlayWorld::fov).unwrap_or(0.0);
        if let Some(fogs) = t.play.world_mut().and_then(PlayWorld::take_zone_fogs) {
            t.shown.fogs = fogs;
        }
        if self.left_canvas_actors.is_empty() {
            t.view.overlays.clear();
            for &(item, slot) in &drawn {
                if let CanvasItem::Actor { fov, .. } = *item
                    && slot == t.view.overlays.len()
                {
                    t.view.overlays.push(fov);
                }
            }
        }
        self.show_canvas_actors(&drawn, t);
        t.times.add("canvas actors", started);
        let started = std::time::Instant::now();
        self.update_projections(t);
        t.times.add("projections", started);
        let started = std::time::Instant::now();
        self.update_material_switches(t);
        t.times.add("material switches", started);
        // Before `update_particles`, which takes the frame's step.
        let started = std::time::Instant::now();
        self.update_fluids(t);
        t.times.add("fluids", started);
        let started = std::time::Instant::now();
        self.update_particles(t);
        t.times.add("particles", started);
    }

    pub fn loading_frame(&mut self, ctx: &egui::Context, rect: egui::Rect, contexts: &mut EguiContexts, t: &mut SceneTargets) {
        let Some(progress) = self.loading() else { return };
        t.play.set_canvas_size([rect.width(), rect.height()]);
        t.view.canvas_below = [false; OVERLAY_LAYERS.len()];
        let items = Arc::clone(&self.last_hud);
        paint_hud(ctx, rect, &items, self, contexts, t);
        if self.progress_bar.is_none() {
            let entry = map_package("Entry");
            let found = self.library.open(&self.root, &entry).ok().and_then(|(game, id)| ProgressBar::read(game, id));
            self.progress_bar = Some(found.map(|(bar, image)| {
                let id = contexts.add_image(EguiTextureHandle::Strong(t.assets.images.add(image)));
                (bar, id)
            }));
        }
        if let Some(Some((bar, texture))) = &self.progress_bar {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, "progress".into())).with_clip_rect(rect);
            painter.add(bar.mesh(rect, progress, *texture));
        }
    }

    /// Viewer-only (and the real-data tests of this crate, which cannot use `p2-viewer`): loads on this thread, blocking.
    /// The game loads maps with [`Session::begin_load`] / [`Session::finish_load`] and never calls this.
    pub fn load_map(&mut self, package: &str, t: &mut SceneTargets) -> Result<String, LoadError> {
        self.clear(t);
        self.start_map(package, t)
    }

    pub fn begin_load(&mut self, package: &str, url: Option<String>, t: &mut SceneTargets) -> Result<(), LoadError> {
        let (game, id) = self.library.open(&self.root, package)?;
        let prepare = scene_preparer(game, id, package, t);
        let map_name = map_name(package);
        // As travel begins the engine draws a few frames more: P2's screens count them (`TravelRenderCount`)
        // and leave their last image up during the load.
        let [width, height] = t.play.canvas_size();
        if url.is_some()
            && let Some(world) = t.play.world_mut()
        {
            // The last of them is the `Entry` map's controller's (the progress bar's): the level's HUD does not draw
            // in it (no HUD, no "PAUSED"), screens draw under the bar in `PreRender`. The image left up is the one
            // before it, where a screen drew over the HUD in `PostRender`.
            for frame in 1..=TRAVEL_FRAMES {
                let items = world.hud(width, height);
                if frame < TRAVEL_FRAMES {
                    self.last_hud = items.into();
                }
            }
        }
        let previous = url.as_ref().and_then(|_| t.play.take_vm());
        let played = previous.as_ref().map(|vm| Loader::clone(vm.loader()));
        let configs = game.configs();
        let progress = Arc::new(LoadProgress::default());
        let thread_progress = Arc::clone(&progress);
        let task = game.spawn_play_loader(map_name, played, Arc::clone(&progress), move |loader, id| {
            let started = std::time::Instant::now();
            let vm = start_vm(previous, url.as_deref(), loader, id, configs, &thread_progress);
            bevy::log::info!("load: VM started in {} ms", started.elapsed().as_millis());
            prepare(vm)
        });
        self.pending = Some(PendingLoad { package: package.to_owned(), progress, task });
        Ok(())
    }

    pub fn map(&self) -> Option<&str> {
        self.map.as_deref()
    }

    pub fn loading(&self) -> Option<f32> {
        self.pending.as_ref().map(|pending| pending.progress.get())
    }

    pub fn finish_load(&mut self, t: &mut SceneTargets) -> Option<Result<String, LoadError>> {
        if !self.pending.as_ref()?.task.is_finished() {
            return None;
        }
        let PendingLoad { package, task, .. } = self.pending.take()?;
        let prepared = match task.join() {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return Some(Err(error)),
            Err(_) => return Some(Err(LoadError::Panicked { package })),
        };
        // The weapon stays on the load screen until the new map's first frame (`present_session`).
        let left: Vec<Entity> = self.canvas_actors.drain(..).map(|shown| shown.entity).collect();
        let started = std::time::Instant::now();
        self.clear(t);
        self.left_canvas_actors = left;
        let shown = self.show_scene(prepared, t);
        bevy::log::info!("load: scene shown in {} ms (main thread)", started.elapsed().as_millis());
        Some(shown)
    }

    fn drop_left_canvas_actors(&mut self, t: &mut SceneTargets) {
        for entity in self.left_canvas_actors.drain(..) {
            t.commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{map_package, start_map};

    #[test]
    fn map_names_become_package_paths() {
        let expected = PathBuf::from("Maps").join("Startup.fuk").display().to_string();
        assert_eq!(map_package("Startup.fuk"), expected);
        assert_eq!(map_package("Startup"), expected);
    }

    #[test]
    fn start_map_skips_what_names_no_map() {
        let root = std::env::temp_dir().join("p2-game-start-map");
        let maps = root.join("Maps");
        std::fs::create_dir_all(&maps).expect("making the test Maps folder");
        std::fs::write(maps.join("Church.fuk"), b"").expect("writing the test map");
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let file = maps.join("Church.fuk").display().to_string();
        let skipped = args(&["C:\\Games\\p2-game.exe", file.as_str(), "-windowed", "+connect", "Game=x", "Nope", "Church"]);
        assert_eq!(start_map(skipped, &root).as_deref(), Some("Church"));
        assert_eq!(start_map(args(&["Church.fuk"]), &root).as_deref(), Some("Church.fuk"));
        assert_eq!(start_map(args(&["Nope", "-log", "x=1"]), &root), None);
        assert_eq!(start_map(args(&[file.as_str()]), &root), None);
        assert_eq!(start_map(Vec::new(), &root), None);
    }
}
