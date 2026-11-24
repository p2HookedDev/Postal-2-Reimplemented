pub mod actors;
pub mod animation;
pub mod diagnostics;
pub mod emitter_changes;
pub mod map;
pub mod play;
pub mod saves;

/// In the order a game host adds them. A host without the game's window and settings (the asset viewer) disables
/// `DisplayPlugin`, `GammaPlugin` and `ModernGraphicsPlugin`.
pub struct P2EnginePlugins;

impl bevy::app::PluginGroup for P2EnginePlugins {
    fn build(self) -> bevy::app::PluginGroupBuilder {
        bevy::app::PluginGroupBuilder::start::<Self>()
            .add(FrameOrderPlugin)
            .add_group(p2_render::P2RenderPlugins)
            .add(p2_host::display::DisplayPlugin)
            .add(p2_render::gamma::GammaPlugin)
            .add(p2_render::modern::ModernGraphicsPlugin)
            .add(animation::AnimationPlugin)
            .add(play::PlayPlugin)
            .add(map::MapPlugin)
            .add(p2_render::wounds::WoundsPlugin)
    }
}

/// The one place where the order between the engine's plugins is set. A plugin orders only its own sets.
pub struct FrameOrderPlugin;

impl bevy::app::Plugin for FrameOrderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::app::{PostUpdate, Update};
        use bevy::ecs::schedule::IntoScheduleConfigs;
        use p2_render::view::MeshViewSystems;
        app.configure_sets(Update, play::PlaySystems::Input.after(MeshViewSystems::Apply))
            .configure_sets(Update, play::PlaySystems::Tick.before(MeshViewSystems::Camera))
            .configure_sets(Update, map::MapSystems::Sprites.in_set(play::PlaySystems::Placed))
            .configure_sets(Update, p2_render::modern::ModernGraphicsSystems::Apply.after(MeshViewSystems::Camera))
            // After the actors' models took their actors' places (`play::follow_actors`), so attachments land on this
            // frame's bones.
            .configure_sets(Update, animation::AnimationSystems::Pose.after(MeshViewSystems::Camera))
            .configure_sets(Update, p2_render::RenderFollowSystems.after(MeshViewSystems::Apply))
            .configure_sets(PostUpdate, p2_render::view::SceneViewSystems::Overlays.after(play::PlaySystems::Present))
            .configure_sets(PostUpdate, p2_render::wounds::WoundsSystems.in_set(map::MapSystems::Present))
            // Wounds age by the step the map's particles take.
            .configure_sets(PostUpdate, map::MapSystems::Particles.after(p2_render::wounds::WoundsSystems))
            .configure_sets(
                PostUpdate,
                p2_render::particle_draw::ParticleDrawSystems.in_set(play::PlaySystems::Present).after(map::MapSystems::Present).run_if(map::map_shown),
            )
            .configure_sets(PostUpdate, p2_render::fire_volume::FireVolumeSystems.after(p2_render::particle_draw::ParticleDrawSystems));
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
            use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
            use p2_render::MainPassExtraSystems;
            // Bounce light gathers and scales the frame's colour, so the light added in front of surfaces comes after it.
            render_app.configure_sets(
                Core3d,
                (MainPassExtraSystems::Bounce, MainPassExtraSystems::FireVolume, MainPassExtraSystems::LampFog)
                    .chain()
                    .in_set(Core3dSystems::MainPass)
                    .after(main_opaque_pass_3d)
                    .before(main_transparent_pass_3d),
            );
        }
    }
}
