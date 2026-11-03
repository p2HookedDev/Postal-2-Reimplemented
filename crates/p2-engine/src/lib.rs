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
            .add(mesh_view::MeshViewPlugin)
            .add(display::DisplayPlugin)
            .add(render::gamma::GammaPlugin)
            .add(render::modern::ModernGraphicsPlugin)
            .add(animation::AnimationPlugin)
            .add(play::PlayPlugin)
    }
}
