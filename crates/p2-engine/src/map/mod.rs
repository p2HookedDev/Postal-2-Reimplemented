use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::asset::{Assets, Handle};
use bevy::camera::visibility::Visibility;
use bevy::ecs::prelude::{AnyOf, ChildOf, Commands, Entity, Has, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::{Mat4, UVec2, Vec2, Vec3, Vec4};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Mesh, MeshTag};
use bevy::transform::components::Transform;
use bevy::camera::Projection;
use bevy_egui::{EguiContexts, EguiGlobalSettings, PrimaryEguiContext, egui};
use rustc_hash::{FxHashMap, FxHashSet};
use p2_assets::{MeshAnimation, Model, SkeletalMesh, StaticMesh, read_font, read_level, read_mesh_animation, read_model, read_static_mesh, read_static_mesh_materials, read_terrain};
use p2_render_cpu::MeshPlacement;
use p2_bevy_assets::{bone_matrices, rotator_matrix, to_bevy, unreal_mesh_matrix, unreal_to_bevy, unreal_transform};
use p2_package::{Ini, LoadedPackage, Loader, Package, PackageId};
use p2_particles::{Emitter, SpriteParticle};
use p2_vm::{CanvasItem, Kind, ObjectId, ParticleCommand, ProjectorEvent, Vm};

use p2_render::lighting::actor_light::{
    ActorLighting, ActorLit, DEFAULT_MAX_LIGHTS, LightEnvironment, MeshLighting, keep_sections_for_relighting, relit_sections,
};
use crate::actors::{
    CoronaActor, EmitterActor, MeshActor, ProjectorActor, SkeletalActor, SpriteActor, antiportal_actor, corona_actor, emitter_actor, location_rotation, mesh_actor, mesh_light,
    projector_actor, skeletal_actor, sprite_actor, zone_ambient, zone_fog,
};
use crate::animation::{Animated, AnimatedMesh};
use p2_render::world::components::FollowBone;
use p2_render::world::components::{LeftCanvasActor, MapScoped};
use p2_bevy_assets::library::{Game, Library, LoadError, LoadProgress, export_reference};
use hud::loading::read_progress_bar;
use p2_render::world::view::{ActorLook, ActorMaterials, ActorModel, CanvasDrawn, FollowActor, MainView, MapModel, OverlaySlot, SceneView, OVERLAY_LAYERS, spawn_actor_sections};
use p2_render::world::scene::{MaterialImages, MovingModel, Scene, SectionView, Sky, LayeredByZone, ZoneCulled};
use p2_render::world::level::{ActorTexture, MaterialSwitches, PendingAssets, SceneAssets, SectionChunks, material_images, projector_image, sections_of_static_mesh, skeletal_sections};
use p2_host::frame_times::FrameTimes;
use crate::play::{Play, PlayWorld};
use p2_render::world::level_collision::LevelCollision;
use p2_render::hud::canvas::CanvasStores;

mod coronas;
mod decal_aging;
mod fluids;
pub(crate) mod liquid_flow;
mod hud;
mod load;
mod movie;
mod particles;
mod scene;
mod spawned;
mod sprites;

use hud::{HudAssets, HudWanted, LibraryReads};
pub use hud::Hud;
pub use hud::canvas::CanvasArea;
pub use load::{LoadMap, MapFailed, MapLoadPlugin, MapState};
use load::{configs, play_loader, spawn_play_loader};
use movie::Movies;
pub use particles::{ParticleReport, Particles};
pub use spawned::SpawnedActors;
use spawned::SpawnTargets;
use hud::canvas_actors::HudParams;
pub use fluids::Fluids;
pub use scene::animate_mesh_with;
use scene::{projector_tag, projector_transform, scene_preparer};
pub use scene::projector_box;

pub struct MapPlugin;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MapSystems {
    Load,
    Paint,
    Present,
    Particles,
    CoronaPlaces,
}

impl bevy::app::Plugin for MapPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        app.add_plugins(MapPresentPlugin).init_resource::<CanvasArea>().add_systems(bevy::app::Startup, hud::canvas::spawn_hud_camera).add_systems(
            bevy_egui::EguiPrimaryContextPass,
            hud::canvas::paint_map_canvas
                .in_set(MapSystems::Paint)
                .after(MapSystems::Load)
                .run_if(bevy::ecs::schedule::common_conditions::resource_exists::<Session>),
        );
    }
}

pub struct MapPresentPlugin;

impl bevy::app::Plugin for MapPresentPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        app.add_plugins(decal_aging::DecalAgingPlugin)
            .init_resource::<Fluids>()
            .init_resource::<MaterialSwitches>()
            .init_resource::<Projectors>()
            .init_resource::<Particles>()
            .init_resource::<SpawnedActors>()
            .init_resource::<Hud>()
            .add_systems(bevy::app::Update, coronas::place_coronas.in_set(MapSystems::CoronaPlaces))
            .add_systems(bevy::app::PostUpdate, spawned::count_pending.after(spawned::present_spawned))
            .configure_sets(bevy::app::PostUpdate, MapSystems::Present.in_set(crate::play::PlaySystems::Present).run_if(map_shown))
            .add_systems(
                bevy::app::PostUpdate,
                (
                    hud::canvas_actors::drop_left_canvas_actors_when_presented,
                    // Spawned and restyled actors first: a HUD-drawn model whose `Skins` changed (the kick's foot getting
                    // bloody) is made again before the HUD's models are shown, not missing for a frame.
                    spawned::present_spawned,
                    hud::canvas_actors::present_canvas_actors,
                    spawned::present_projections,
                    scene::present_material_switches,
                    fluids::present_fluids,
                    particles::present_particles.in_set(MapSystems::Particles),
                )
                    // Spawns and despawns queued by one part reach the world after the last part, as when all were one system.
                    .chain_ignore_deferred()
                    .in_set(MapSystems::Present),
            );
    }
}

#[derive(SystemParam)]
pub struct SceneTargets<'w, 's> {
    pub assets: AssetStores<'w>,
    pub(crate) models: ActorModels<'w, 's>,
    pub scenes: bevy::ecs::prelude::MessageWriter<'w, p2_render::world::scene::SceneChange>,
    pub animated: ResMut<'w, Animated>,
    pub times: ResMut<'w, FrameTimes>,
    pub play: ResMut<'w, Play>,
    pub commands: Commands<'w, 's>,
    pub(crate) fluids: ResMut<'w, Fluids>,
    pub(crate) switches: ResMut<'w, MaterialSwitches>,
    pub(crate) projectors: ResMut<'w, Projectors>,
    pub(crate) particles: ResMut<'w, Particles>,
    pub(crate) particle_frame: ResMut<'w, p2_render::effects::particle_frame::DrawnParticles>,
    pub(crate) spawned: ResMut<'w, SpawnedActors>,
    pub(crate) hud: ResMut<'w, Hud>,
    pub(crate) painter: ResMut<'w, p2_render::hud::canvas::CanvasPainter>,
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
    pub actor_materials: ActorMaterials<'w>,
    pub sprite_buffers: Option<ResMut<'w, Assets<bevy::render::storage::ShaderBuffer>>>,
    pub audio: Option<ResMut<'w, Assets<bevy::audio::AudioSource>>>,
    pub bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
    pub wet: ResMut<'w, p2_render::effects::wet::WetTextures>,
}

impl SceneAssets for AssetStores<'_> {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        self.meshes.add(mesh)
    }

    fn add_image(&mut self, image: Image) -> Handle<Image> {
        self.images.add(image)
    }

    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes> {
        self.bindposes.add(bindposes)
    }

    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2> {
        self.images.get(image).map(Image::size)
    }
}

#[derive(SystemParam)]
pub(crate) struct ParticleView<'w, 's> {
    pub(crate) collision: Res<'w, LevelCollision>,
    pub(crate) cameras: Query<'w, 's, &'static Transform, With<SceneView>>,
    pub(crate) projections: Query<'w, 's, &'static Projection, With<SceneView>>,
    pub(crate) zone_culled: Query<'w, 's, &'static Visibility, With<ZoneCulled>>,
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
        (Entity, &'static FollowActor, &'static mut Transform, &'static mut Visibility, Option<&'static OverlaySlot>),
        (Without<CanvasDrawn>, Without<SceneView>, Without<ZoneCulled>),
    >,
}

#[derive(SystemParam)]
pub(crate) struct ActorModels<'w, 's> {
    pub(crate) followers: Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>, Option<&'static ChildOf>), Without<CanvasDrawn>>,
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

pub(crate) fn not_loading(session: Option<Res<Session>>) -> bool {
    session.is_none_or(|session| session.loading().is_none())
}

pub(crate) fn map_shown(session: Option<Res<Session>>, play: Option<Res<Play>>) -> bool {
    session.is_some_and(|session| session.loading().is_none()) && play.is_some_and(|play| play.world().is_some())
}

const TRAVEL_FRAMES: usize = 5;

struct PendingLoad {
    package: String,
    progress: Arc<LoadProgress>,
    task: std::thread::JoinHandle<Result<scene::PreparedScene, LoadError>>,
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
    progress.set(0.03);
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
            for (retail, ours) in [("MenuOptions", "ModernMenuOptions"), ("MenuMain", "ModernMenuMain"), ("MenuGame", "ModernMenuGame")] {
                if vm.redirect_class(("Shell", retail), ("P2Modern", ours)) {
                    bevy::log::info!("modern menu: P2Modern.{ours} found, {retail} swapped");
                } else {
                    bevy::log::info!("modern menu: P2Modern.{ours} not found, retail {retail}");
                }
            }
            vm.options.steam_apps = p2_host::install::steam_installed_apps();
            vm
        }
    };
    crate::play::photo::clear_photo_active(&mut vm);
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
    progress.set(0.08);
    if fresh && let Err(error) = vm.init_client().and_then(|()| vm.add_viewport_interaction("P2Modern", "ModernPhotoMode")) {
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
    progress.set(scene::SCENE_START);
    StartedVm { vm, loader, id, script_errors, player }
}

fn open_game<'a>(library: &'a mut Library, system: Option<&Path>) -> Option<&'a mut Game> {
    library.game(system?)
}

fn map_name(package: &str) -> &str {
    Path::new(package).file_stem().and_then(|s| s.to_str()).unwrap_or_default()
}

impl Session {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            library: Library::new(p2_host::game_dir::config_folder()),
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
        let map = self.map.take();
        if let Some(map) = map.as_deref() {
            t.painter.forget_map(map_name(map));
        }
        self.map_system = None;
        t.hud.leave_map(map.as_deref().map(map_name), &mut t.commands);
        t.commands.queue(despawn_map_scoped);
        *t.projectors = Projectors::default();
        *t.spawned = SpawnedActors::default();
        t.assets.actor_materials.forget_spawned();
        *t.particles = Particles::default();
        t.particle_frame.leave_level();
        *t.fluids = Fluids::default();
    }

    pub fn load_map(&mut self, package: &str, t: &mut SceneTargets) -> Result<String, LoadError> {
        self.clear(t);
        self.start_map(package, t)
    }

    pub fn begin_load(&mut self, package: &str, url: Option<String>, t: &mut SceneTargets) -> Result<(), LoadError> {
        let (game, id) = self.library.open(&self.root, package)?;
        let prepare = scene_preparer(game, id, package, t);
        let map_name = map_name(package);
        let canvas = t.play.canvas();
        if url.is_some()
            && let Some(world) = t.play.world_mut()
        {
            for frame in 1..=TRAVEL_FRAMES {
                let items = world.hud(canvas);
                if frame < TRAVEL_FRAMES {
                    t.hud.last_hud = items.into();
                }
            }
        }
        let previous = url.as_ref().and_then(|_| t.play.take_vm());
        let played = previous.as_ref().map(|vm| Loader::clone(vm.loader()));
        let configs = configs(game);
        let progress = Arc::new(LoadProgress::default());
        let thread_progress = Arc::clone(&progress);
        let task = spawn_play_loader(game, map_name, played, Arc::clone(&progress), move |loader, id| {
            let started = std::time::Instant::now();
            let vm = start_vm(previous, url.as_deref(), loader, id, configs, &thread_progress);
            bevy::log::info!("load: VM started in {} ms", started.elapsed().as_millis());
            prepare(vm, &thread_progress)
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
