pub mod actors;
pub mod animation;
mod components;
pub mod diagnostics;
pub mod display;
pub mod emitter_changes;
pub mod game_dir;
pub mod game_dir_form;
pub mod install;
mod level_collision;
pub mod library;
mod loading;
pub mod map;
pub mod mesh_view;
pub mod play;
pub mod render;
pub mod saves;
mod shown_scene;

/// In the order a game host adds them. A host without the game's window and settings (the asset viewer) disables
/// `DisplayPlugin`, `GammaPlugin` and `ModernGraphicsPlugin`.
pub struct P2EnginePlugins;

impl bevy::app::PluginGroup for P2EnginePlugins {
    fn build(self) -> bevy::app::PluginGroupBuilder {
        bevy::app::PluginGroupBuilder::start::<Self>()
            .add(FrameOrderPlugin)
            .add(render::RetailRenderPlugin)
            .add(mesh_view::MeshViewPlugin)
            .add(display::DisplayPlugin)
            .add(render::gamma::GammaPlugin)
            .add(render::modern::ModernGraphicsPlugin)
            .add(animation::AnimationPlugin)
            .add(play::PlayPlugin)
            .add(map::MapPlugin)
    }
}

/// The one place where the order between the engine's plugins is set. A plugin orders only its own sets.
pub struct FrameOrderPlugin;

impl bevy::app::Plugin for FrameOrderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::app::Update;
        use bevy::ecs::schedule::IntoScheduleConfigs;
        use mesh_view::MeshViewSystems;
        app.configure_sets(Update, play::PlaySystems::Input.after(MeshViewSystems::Apply))
            .configure_sets(Update, play::PlaySystems::Tick.before(MeshViewSystems::Camera))
            .configure_sets(Update, render::modern::ModernGraphicsSystems::Apply.after(MeshViewSystems::Camera))
            // After the actors' models took their actors' places (`play::follow_actors`), so attachments land on this
            // frame's bones.
            .configure_sets(Update, animation::AnimationSystems::Pose.after(MeshViewSystems::Camera));
    }
}

pub(crate) fn add_once<P: bevy::app::Plugin>(app: &mut bevy::app::App, plugin: P) {
    if !app.is_plugin_added::<P>() {
        app.add_plugins(plugin);
    }
}
