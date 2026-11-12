use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers, Visibility};
use bevy::ecs::prelude::{AnyOf, ChildOf, Commands, Entity, Has, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::material::AlphaMode;
use bevy::math::{Mat4, UVec2, Vec2, Vec3, Vec4};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Indices, Mesh, Mesh3d, MeshTag, PrimitiveTopology};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
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
use crate::components::{LeftCanvasActor, MapScoped};
use crate::library::{Game, Library, LoadError, LoadProgress, PlayLoaderTask};
use crate::loading::ProgressBar;
use crate::render::translucent_order::TranslucentDraw;
use crate::mesh_view::{
    ActorModel, CanvasDrawn, FollowActor, MaterialImages, MeshView, ModelView, MapModel, MovingModel, Scene, SectionMaterial, SectionView, Sky, SceneView, Skin,
    LayeredByZone, ZoneCulled, OVERLAY_LAYERS, bounds, merged_box, spawn_actor_sections,
};
use crate::render::mesh_light::MeshLitMaterial;
use crate::render::particle_material::ParticleMaterial;
use crate::play::{FrameTimes, Overlay, OverlayTitle, Play, PlayWorld};
pub use crate::level_collision::{LevelCollision, StaticCollision};

mod canvas_actors;
mod coronas;
mod fluids;
mod hud;
mod load;
mod movie;
mod particles;
mod scene;
mod spawned;
mod sprites;

use hud::{HudAssets, paint_canvas, paint_hud, paint_overlay};
pub use hud::Hud;
pub use load::{LoadMap, MapFailed, MapLoadPlugin, MapState};
use movie::Movies;
pub use particles::{ParticleReport, Particles};
pub use particles::pad_bounds;
pub use sprites::{SpriteQuad, face_sprites};
pub use coronas::{CoronaActor, corona_actor, draw_coronas, is_corona};
pub use spawned::{SpawnedActors, material_images};
use spawned::SpawnTargets;
use canvas_actors::HudTargets;
pub use fluids::Fluids;
pub use scene::MaterialSwitches;
pub use scene::{SceneAssets, SectionChunks, animate_mesh_with, sections_of_static_mesh, skeletal_mesh_sections, skeletal_sections};
use scene::{ActorTexture, projector_image, projector_tag, projector_transform, scene_preparer};
pub use scene::projector_box;

pub struct MapPlugin;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MapSystems {
    /// In the egui pass: a host that paints a map's frame does it after this.
    Load,
    Present,
}

impl bevy::app::Plugin for MapPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        app.init_resource::<Fluids>()
            .init_resource::<MaterialSwitches>()
            .init_resource::<Projectors>()
            .init_resource::<Particles>()
            .init_resource::<SpawnedActors>()
            .init_resource::<Hud>()
            .add_systems(bevy::app::Startup, spawn_hud_camera)
            .configure_sets(bevy::app::PostUpdate, MapSystems::Present.in_set(crate::play::PlaySystems::Present).run_if(map_shown))
            .add_systems(
                bevy::app::PostUpdate,
                (
                    canvas_actors::drop_left_canvas_actors_when_presented,
                    // Spawned and restyled actors first: a HUD-drawn model whose `Skins` changed (the kick's foot getting
                    // bloody) is made again before the HUD's models are shown, not missing for a frame.
                    spawned::present_spawned,
                    canvas_actors::present_canvas_actors,
                    spawned::present_projections,
                    scene::present_material_switches,
                    // Before `update_particles`, which takes the frame's step.
                    fluids::present_fluids,
                    particles::present_particles,
                )
                    // Spawns and despawns queued by one part reach the world after the last part, as when all were one system.
                    .chain_ignore_deferred()
                    .in_set(MapSystems::Present),
            );
    }
}

fn spawn_hud_camera(mut commands: Commands, mut egui_settings: ResMut<EguiGlobalSettings>) {
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
    pub scenes: bevy::ecs::prelude::MessageWriter<'w, crate::mesh_view::SceneChange>,
    pub(crate) shown: ResMut<'w, crate::shown_scene::ShownScene>,
    pub animated: ResMut<'w, Animated>,
    pub times: ResMut<'w, FrameTimes>,
    pub play: ResMut<'w, Play>,
    pub commands: Commands<'w, 's>,
    pub(crate) fluids: ResMut<'w, Fluids>,
    pub(crate) switches: ResMut<'w, MaterialSwitches>,
    pub(crate) projectors: ResMut<'w, Projectors>,
    pub(crate) particles: ResMut<'w, Particles>,
    pub(crate) spawned: ResMut<'w, SpawnedActors>,
    pub(crate) hud: ResMut<'w, Hud>,
}

impl<'w, 's> SceneTargets<'w, 's> {
    fn spawn_targets(&mut self) -> SpawnTargets<'_, 'w, 's> {
        SpawnTargets {
            assets: &mut self.assets,
            spawned: &mut self.spawned,
            particles: &mut self.particles,
            hud: &mut self.hud,
            animated: &mut self.animated,
            play: &mut self.play,
            actor_models: &self.models.actor_models,
            followers: &self.models.followers,
            commands: &mut self.commands,
            times: &mut self.times,
        }
    }
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
    pub(crate) pool_slots: Query<'w, 's, &'static mut Transform, (With<particles::PoolSlot>, Without<FollowActor>, Without<SceneView>)>,
}

fn frustum_test(
    cameras: &Query<&'static Transform, With<SceneView>>,
    projections: &Query<&'static Projection, With<SceneView>>,
    origin: Vec3,
) -> impl Fn(Option<(Vec3, Vec3)>) -> bool + use<> {
    let camera = cameras.iter().next();
    let frustum = camera.zip(projections.iter().next()).map(|(camera, projection)| projection.compute_frustum(&bevy::transform::components::GlobalTransform::from(*camera)));
    let at_origin = bevy::math::Affine3A::from_translation(origin);
    move |bounds| match (bounds, &frustum) {
        (Some((min, max)), Some(frustum)) => frustum.intersects_obb(&bevy::camera::primitives::Aabb::from_min_max(min, max), &at_origin, true, true),
        _ => true,
    }
}

#[derive(SystemParam)]
pub(crate) struct CanvasModels<'w, 's> {
    pub(crate) canvas_models: Query<'w, 's, (&'static FollowActor, &'static CanvasDrawn, &'static mut Transform), Without<SceneView>>,
    pub(crate) canvas_sections: Query<'w, 's, (Entity, &'static ChildOf), With<CanvasDrawn>>,
    pub(crate) left: Query<'w, 's, (), With<LeftCanvasActor>>,
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

#[derive(Resource)]
pub struct Session {
    root: PathBuf,
    pub library: Library,
    map: Option<String>,
    map_system: Option<PathBuf>,
    pending: Option<PendingLoad>,
    animations: HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    vm_animations: scene::VmAnimations,
}

#[derive(Resource, Default)]
pub struct Projectors {
    dynamic_decals: FxHashMap<ObjectId, Entity>,
    mover_decals: FxHashMap<ObjectId, spawned::MoverDecal>,
    decal_movers: FxHashSet<ObjectId>,
    projector_actors: FxHashSet<ObjectId>,
    decal_images: FxHashMap<String, Option<(Handle<Image>, Vec2, Option<p2_assets::FinalBlend>)>>,
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

fn despawn_map_scoped(world: &mut bevy::ecs::world::World) {
    let scoped: Vec<Entity> = world.query_filtered::<Entity, bevy::ecs::query::Or<(With<MapScoped>, With<LeftCanvasActor>)>>().iter(world).collect();
    for entity in scoped {
        if let Ok(entity) = world.get_entity_mut(entity) {
            entity.despawn();
        }
    }
}

fn map_shown(session: Option<Res<Session>>, play: Res<Play>) -> bool {
    session.is_some_and(|session| session.loading().is_none()) && play.world().is_some()
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
            for (retail, ours) in [("MenuOptions", "ModernMenuOptions"), ("MenuMain", "ModernMenuMain")] {
                if vm.redirect_class(("Shell", retail), ("P2Modern", ours)) {
                    bevy::log::info!("modern menu: P2Modern.{ours} found, {retail} swapped");
                } else {
                    bevy::log::info!("modern menu: P2Modern.{ours} not found, retail {retail}");
                }
            }
            vm.options.steam_apps = crate::install::steam_installed_apps();
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
            pending: None,
            animations: HashMap::new(),
            vm_animations: scene::VmAnimations::default(),
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
        let hud = &mut *t.hud;
        hud.hud_textures.clear();
        hud.hud_fonts.clear();
        hud.hud_images.clear();
        hud.hud_assets = HudAssets::default();
        hud.movies.stop(&mut t.commands);
        hud.canvas_actors.clear();
        hud.failed_canvas_meshes.clear();
        t.commands.queue(despawn_map_scoped);
        *t.projectors = Projectors::default();
        *t.spawned = SpawnedActors::default();
        let particles = &mut *t.particles;
        particles.waiting_sprites.clear();
        particles.waiting_coronas.clear();
        particles.actors.clear();
        particles.particle_emitters.clear();
        particles.pool_materials.clear();
        particles.emitter_materials.clear();
        particles.corona_materials.clear();
        *t.fluids = Fluids::default();
    }

    pub fn frame(&mut self, ctx: &egui::Context, rect: egui::Rect, contexts: &mut EguiContexts, t: &mut SceneTargets) {
        let started = std::time::Instant::now();
        t.hud.start_frame(started);
        t.play.set_canvas_size([rect.width(), rect.height()]);
        t.view.canvas_below = [false; OVERLAY_LAYERS.len()];
        // Until a new map's first VM frame is presented, the last image (the load screen) stays up, as in the
        // engine's frame buffer; its canvas actors belong to the level left.
        if t.play.world().is_some_and(|world| !world.presented()) {
            let items = Arc::clone(&t.hud.last_hud);
            paint_hud(ctx, rect, &items, self, contexts, t);
            t.times.add("hud paint", started);
            return;
        }
        let items = t.play.world().map(PlayWorld::hud_items).unwrap_or_default();
        let hud = &mut *t.hud;
        hud.movies.update(&self.root, contexts, &mut t.play, &mut t.commands, &mut t.assets, &mut hud.hud_textures);
        if let Some(overlay) = t.play.world().and_then(PlayWorld::screen_overlay) {
            paint_overlay(ctx, rect, overlay, self, contexts, t);
        }
        if !items.is_empty() {
            paint_canvas(ctx, rect, &items, self, contexts, t);
        }
        t.times.add("hud paint", started);
        t.hud.last_hud = items;
    }

    /// Runs the parts of `MapSystems::Present` in their order, for hosts that keep a `Session` outside the app's resources.
    pub fn present(&mut self, items: &[CanvasItem], t: &mut SceneTargets) {
        let started = std::time::Instant::now();
        self.update_spawned(&mut t.spawn_targets());
        t.times.add("spawned", started);
        let started = std::time::Instant::now();
        self.show_hud_actors(items, &mut t.hud_targets());
        t.times.add("canvas actors", started);
        let started = std::time::Instant::now();
        spawned::update_projections(&mut spawned::ProjectionTargets {
            library: &mut self.library,
            map_system: self.map_system.as_deref(),
            projectors: &mut t.projectors,
            play: &mut t.play,
            followers: &t.models.followers,
            untagged: &t.models.untagged,
            images: &mut t.assets.images,
            commands: &mut t.commands,
        });
        t.times.add("projections", started);
        let started = std::time::Instant::now();
        scene::update_material_switches(&mut scene::SwitchTargets {
            library: &mut self.library,
            map_system: self.map_system.as_deref(),
            switches: &mut t.switches,
            play: &mut t.play,
            images: &mut t.assets.images,
            commands: &mut t.commands,
        });
        t.times.add("material switches", started);
        let started = std::time::Instant::now();
        fluids::update_fluids(&mut fluids::FluidTargets {
            fluids: &mut t.fluids,
            play: &mut t.play,
            cameras: &t.particle_view.cameras,
            projections: &t.particle_view.projections,
            commands: &mut t.commands,
            meshes: &mut t.assets.meshes,
            materials: &mut t.assets.materials,
            mesh_lit: &mut t.assets.mesh_lit,
        });
        t.times.add("fluids", started);
        let started = std::time::Instant::now();
        particles::update_particles(&mut particles::ParticleTargets {
            particles: &mut t.particles,
            play: &mut t.play,
            canvas: &t.hud.canvas_actors,
            view: &mut t.particle_view,
            commands: &mut t.commands,
            meshes: &mut t.assets.meshes,
            sprite_buffers: t.assets.sprite_buffers.as_mut(),
        });
        t.times.add("particles", started);
    }

    pub fn loading_frame(&mut self, ctx: &egui::Context, rect: egui::Rect, contexts: &mut EguiContexts, t: &mut SceneTargets) {
        let Some(progress) = self.loading() else { return };
        t.play.set_canvas_size([rect.width(), rect.height()]);
        t.view.canvas_below = [false; OVERLAY_LAYERS.len()];
        let items = Arc::clone(&t.hud.last_hud);
        paint_hud(ctx, rect, &items, self, contexts, t);
        if t.hud.progress_bar.is_none() {
            let entry = map_package("Entry");
            let found = self.library.open(&self.root, &entry).ok().and_then(|(game, id)| ProgressBar::read(game, id));
            t.hud.progress_bar = Some(found.map(|(bar, image)| {
                let id = contexts.add_image(EguiTextureHandle::Strong(t.assets.images.add(image)));
                (bar, id)
            }));
        }
        if let Some(Some((bar, texture))) = &t.hud.progress_bar {
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
                    t.hud.last_hud = items.into();
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
        // The weapon stays on the load screen until the new map's first frame (`drop_left_canvas_actors_when_presented`).
        let left: Vec<Entity> = t.hud.canvas_actors.drain(..).map(|shown| shown.entity).collect();
        for &entity in &left {
            t.commands.entity(entity).remove::<MapScoped>();
        }
        let started = std::time::Instant::now();
        self.clear(t);
        for entity in left {
            t.commands.entity(entity).insert(LeftCanvasActor);
        }
        let shown = self.show_scene(prepared, t);
        bevy::log::info!("load: scene shown in {} ms (main thread)", started.elapsed().as_millis());
        Some(shown)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{LeftCanvasActor, MapScoped, despawn_map_scoped, map_package, start_map};

    #[test]
    fn map_names_become_package_paths() {
        let expected = PathBuf::from("Maps").join("Startup.fuk").display().to_string();
        assert_eq!(map_package("Startup.fuk"), expected);
        assert_eq!(map_package("Startup"), expected);
    }

    #[test]
    fn leaving_a_map_despawns_what_was_made_for_it() {
        let mut world = bevy::ecs::world::World::new();
        let root = world.spawn(MapScoped).id();
        let child = world.spawn(bevy::ecs::prelude::ChildOf(root)).id();
        let left = world.spawn(LeftCanvasActor).id();
        let kept = world.spawn_empty().id();
        despawn_map_scoped(&mut world);
        assert!([root, child, left].iter().all(|&entity| world.get_entity(entity).is_err()));
        assert!(world.get_entity(kept).is_ok());
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
