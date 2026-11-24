use super::*;
use bevy::app::{App, Plugin};
use bevy::ecs::prelude::{Message, MessageReader, MessageWriter};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::resource_exists;
use bevy::state::prelude::{AppExtStates, NextState, States, in_state};
use bevy_egui::EguiPrimaryContextPass;
use p2_package::ResolveError;

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MapState {
    #[default]
    Empty,
    Loading,
    Shown,
}

/// `url` is the travel a script asked for; without one the map starts fresh.
#[derive(Message, Debug, Clone)]
pub struct LoadMap {
    pub package: String,
    pub url: Option<String>,
}

/// The map could not be loaded or its script thread stopped. The text is for the player.
#[derive(Message, Debug, Clone)]
pub struct MapFailed(pub String);

/// The asset viewer loads on its own thread and does not add this.
pub struct MapLoadPlugin;

impl Plugin for MapLoadPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<MapState>().add_message::<LoadMap>().add_message::<MapFailed>().add_systems(
            EguiPrimaryContextPass,
            (request_travel, begin_loads, finish_loads.run_if(in_state(MapState::Loading)), report_lost_vm, drop_failed_map)
                .chain()
                .in_set(MapSystems::Load)
                .run_if(resource_exists::<Session>),
        );
    }
}

impl Session {
    fn loading_package(&self) -> Option<&str> {
        self.pending.as_ref().map(|pending| pending.package.as_str())
    }
}

fn request_travel(mut play: ResMut<Play>, session: Res<Session>, mut loads: MessageWriter<LoadMap>) {
    let Some(url) = play.world_mut().and_then(PlayWorld::take_travel) else { return };
    // A URL that names no map restarts the one shown.
    let Some(package) = travel_package(&url).or_else(|| session.map().map(str::to_owned)) else { return };
    loads.write(LoadMap { package, url: Some(url) });
}

fn begin_loads(
    mut loads: MessageReader<LoadMap>,
    mut session: ResMut<Session>,
    mut t: SceneTargets,
    mut next: ResMut<NextState<MapState>>,
    mut failed: MessageWriter<MapFailed>,
) {
    let Some(LoadMap { package, url }) = loads.read().last().cloned() else { return };
    match session.begin_load(&package, url, &mut t) {
        Ok(()) => next.set(MapState::Loading),
        Err(error) => {
            failed.write(MapFailed(format!("{package} could not be loaded: {error}")));
            next.set(MapState::Empty);
        }
    }
}

fn finish_loads(mut session: ResMut<Session>, mut t: SceneTargets, mut next: ResMut<NextState<MapState>>, mut failed: MessageWriter<MapFailed>) {
    let package = session.loading_package().unwrap_or_default().to_owned();
    match session.finish_load(&mut t) {
        None => {}
        Some(Ok(_)) => {
            if let Some(world) = t.play.world_mut() {
                world.hold_reveal();
            }
            next.set(MapState::Shown);
        }
        Some(Err(error)) => {
            failed.write(MapFailed(format!("{package} could not be loaded: {error}")));
            next.set(MapState::Empty);
        }
    }
}

fn report_lost_vm(play: Res<Play>, session: Res<Session>, mut next: ResMut<NextState<MapState>>, mut failed: MessageWriter<MapFailed>) {
    if play.world().is_some_and(PlayWorld::vm_lost) {
        let map = session.map().unwrap_or_default();
        failed.write(MapFailed(format!("{map} stopped: the game's script thread crashed. Details are in diagnostics.txt.")));
        next.set(MapState::Empty);
    }
}

fn drop_failed_map(mut failed: MessageReader<MapFailed>, mut session: ResMut<Session>, mut t: SceneTargets) {
    if failed.read().last().is_some() {
        session.clear(&mut t);
        t.scenes.write(p2_render::scene::SceneChange::Clear);
    }
}

pub(super) fn play_loader(game: &Game, map: &str) -> Result<(Arc<Loader>, PackageId), ResolveError> {
    Game::add_map(Game::script_loader(&game.system, p2_host::game_dir::config_folder())?, map, &LoadProgress::default())
}

/// After travel, `played` with the new map added, so every package keeps its id (`Vm::set_loader`). Package loading
/// reports 0 to 0.45 to `progress`.
pub(super) fn spawn_play_loader<T: Send + 'static>(
    game: &Game,
    map: &str,
    played: Option<Loader>,
    progress: Arc<LoadProgress>,
    then: impl FnOnce(Arc<Loader>, PackageId) -> Result<T, LoadError> + Send + 'static,
) -> std::thread::JoinHandle<Result<T, LoadError>> {
    let (system, map) = (game.system.clone(), map.to_owned());
    std::thread::Builder::new()
        .name("map loading".into())
        // The VM's script recursion (`start_vm` runs the map's start-up scripts here).
        .stack_size(p2_vm::SCRIPT_STACK)
        .spawn(move || {
            let started = std::time::Instant::now();
            let loader = match played {
                Some(loader) => loader,
                None => Game::script_loader(&system, p2_host::game_dir::config_folder())?,
            };
            let (loader, id) = Game::add_map(loader, &map, &progress)?;
            bevy::log::info!("load: {map} packages read in {} ms", started.elapsed().as_millis());
            then(loader, id)
        })
        .map_err(LoadError::Thread)
        .unwrap_or_else(|error| std::thread::spawn(move || Err(error)))
}

pub(super) fn configs(game: &Game) -> Vec<(&'static str, Ini)> {
    let read = |names: &[&str]| {
        names.iter().find_map(|name| std::fs::read(game.system.join(name)).ok()).map(|bytes| Ini::from_bytes(&bytes))
    };
    [("System", read(&["Postal2.ini", "Default.ini"])), ("User", read(&["User.ini", "DefUser.ini"])), ("Selectors", read(&["Selectors.ini"])), ("Subtitles", read(&["Subtitles.ini"])), ("Settings", Some(p2_host::display::load_settings()))]
        .into_iter()
        .filter_map(|(name, ini)| Some((name, ini?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetApp;
    use bevy::ecs::message::Messages;
    use bevy::state::prelude::State;

    fn host() -> App {
        let root = std::env::temp_dir().join("p2-engine-no-such-install");
        let mut app = App::new();
        app.add_plugins((
            bevy::MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::transform::TransformPlugin,
            bevy::state::app::StatesPlugin,
            p2_render::P2RenderPlugins,
            crate::play::PlayPlugin,
            crate::animation::AnimationPlugin,
            MapLoadPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<bevy::pbr::StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_asset::<bevy::audio::AudioSource>()
        .init_resource::<Fluids>()
        .init_resource::<MaterialSwitches>()
        .init_resource::<Projectors>()
        .init_resource::<Particles>()
        .init_resource::<SpawnedActors>()
        .init_resource::<Hud>()
        .insert_resource(Session::new(root));
        app
    }

    #[test]
    fn a_map_that_cannot_be_opened_fails_is_dropped_and_no_map_is_loading() {
        let mut app = host();
        app.world_mut().write_message(LoadMap { package: map_package("Nope"), url: None });
        let level = app.world().resource::<p2_render::particle_frame::DrawnParticles>().level;
        app.world_mut().run_schedule(EguiPrimaryContextPass);
        assert_ne!(app.world().resource::<p2_render::particle_frame::DrawnParticles>().level, level);
        let failures: Vec<String> = app.world_mut().resource_mut::<Messages<MapFailed>>().drain().map(|failed| failed.0).collect();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("Nope"), "{failures:?}");
        app.update();
        assert_eq!(*app.world().resource::<State<MapState>>().get(), MapState::Empty);
        assert!(app.world().resource::<Session>().loading().is_none());
    }

    #[test]
    fn without_an_area_the_canvas_is_left_alone() {
        let mut app = host();
        app.init_resource::<CanvasArea>().init_resource::<bevy_egui::EguiUserTextures>().add_systems(EguiPrimaryContextPass, canvas::paint_map_canvas);
        let canvas = app.world().resource::<Play>().canvas();
        app.world_mut().run_schedule(EguiPrimaryContextPass);
        assert_eq!(app.world().resource::<Play>().canvas(), canvas);
    }
}
