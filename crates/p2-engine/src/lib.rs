pub mod actors;
pub mod animation;
pub mod diagnostics;
pub mod emitter_changes;
pub mod map;
pub mod play;
pub mod saves;

pub struct P2EnginePlugins;

impl bevy::app::PluginGroup for P2EnginePlugins {
    fn build(self) -> bevy::app::PluginGroupBuilder {
        bevy::app::PluginGroupBuilder::start::<Self>()
            .add(FrameOrderPlugin)
            .add_group(p2_render::P2RenderPlugins)
            .add(p2_host::settings::DisplayPlugin)
            .add(animation::AnimationPlugin)
            .add(play::PlayPlugin)
            .add(map::MapPlugin)
    }
}

pub struct FrameOrderPlugin;

impl bevy::app::Plugin for FrameOrderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::app::{PostUpdate, Update};
        use bevy::ecs::schedule::IntoScheduleConfigs;
        use p2_render::world::view::MainViewSystems;
        app.configure_sets(Update, play::PlaySystems::Input.after(MainViewSystems::Apply))
            .configure_sets(Update, play::PlaySystems::Tick.before(MainViewSystems::Camera))
            .configure_sets(Update, p2_render::effects::sprites::SpriteSystems.in_set(play::PlaySystems::Placed))
            .configure_sets(Update, map::MapSystems::CoronaPlaces.in_set(play::PlaySystems::Placed).before(p2_render::lighting::coronas::CoronaSystems))
            .configure_sets(Update, p2_render::lighting::coronas::CoronaSystems.in_set(play::PlaySystems::Placed))
            .configure_sets(Update, animation::AnimationSystems::Pose.after(MainViewSystems::Camera).run_if(map::not_loading))
            .configure_sets(PostUpdate, p2_render::world::view::SceneViewSystems::Overlays.after(play::PlaySystems::Present))
            .configure_sets(PostUpdate, p2_render::effects::wounds::WoundsSystems.in_set(map::MapSystems::Present))
            .configure_sets(PostUpdate, map::MapSystems::Particles.after(p2_render::effects::wounds::WoundsSystems))
            .configure_sets(
                PostUpdate,
                p2_render::effects::particle_draw::ParticleDrawSystems.in_set(play::PlaySystems::Present).after(map::MapSystems::Present).run_if(map::map_shown),
            )
            .configure_sets(PostUpdate, p2_render::effects::fluids::FluidSystems.in_set(play::PlaySystems::Present).after(map::MapSystems::Present));
    }
}
