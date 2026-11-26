pub mod world;
pub mod lighting;
pub mod shadows;
pub mod effects;
pub mod post;
pub mod hud;
pub mod gpu;
pub mod asset_churn;
pub mod modern;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MainPassExtraSystems {
    Bounce,
    FireVolume,
    LampFog,
    GpuParticles,
    Liquids,
}

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderFollowSystems;

pub struct P2RenderPlugins;

impl bevy::app::PluginGroup for P2RenderPlugins {
    fn build(self) -> bevy::app::PluginGroupBuilder {
        bevy::app::PluginGroupBuilder::start::<Self>()
            .add(RetailRenderPlugin)
            .add(RetailLightPlugin)
            .add(world::view::MainViewPlugin)
            .add(post::gamma::GammaPlugin)
            .add(modern::ModernGraphicsPlugin)
            .add(effects::wounds::WoundsPlugin)
    }
}

pub struct RetailRenderPlugin;

impl bevy::app::Plugin for RetailRenderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::app::{PostUpdate, Update};
        use bevy::ecs::schedule::IntoScheduleConfigs;
        use world::view::MainViewSystems;
        app.init_resource::<world::frame_facts::FrameFacts>();
        app.configure_sets(Update, modern::ModernGraphicsSystems::Apply.after(MainViewSystems::Camera))
            .configure_sets(Update, RenderFollowSystems.after(MainViewSystems::Apply))
            .configure_sets(PostUpdate, effects::fire_volume::FireVolumeSystems.after(effects::particle_draw::ParticleDrawSystems));
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
            use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
            render_app.configure_sets(
                Core3d,
                (MainPassExtraSystems::Bounce, MainPassExtraSystems::FireVolume, MainPassExtraSystems::LampFog, MainPassExtraSystems::GpuParticles, MainPassExtraSystems::Liquids)
                    .chain()
                    .in_set(Core3dSystems::MainPass)
                    .after(main_opaque_pass_3d)
                    .before(main_transparent_pass_3d),
            );
        }
        app.add_plugins((
            world::modern_view::ModernViewPlugin,
            post::reflections::ReflectionsPlugin,
            lighting::lamp_fog::LampFogPlugin,
            (lighting::bounce::BounceLightPlugin, post::modern_bloom::ModernBloomPlugin),
            effects::fire_volume::FireVolumePlugin,
            post::temporal_aa::TemporalAaPlugin,
            effects::particle_material::ParticlePlugin,
            effects::particle_draw::ParticleDrawPlugin,
            world::blended::BlendedPlugin,
            world::translucent_order::TranslucentOrderPlugin,
            world::lightmapped::LightmappedPlugin,
            lighting::actor_light::ActorLightPlugin,
            world::mesh_light::MeshLightPlugin,
            world::world_pass::WorldPassPlugin,
            gpu::sampler_refresh::SamplerRefreshPlugin,
        ));
        app.add_plugins((effects::sprites::SpritePlugin, lighting::coronas::CoronaPlugin, effects::fluids::FluidPlugin, effects::gpu_particles::GpuParticlesPlugin, effects::debris::DebrisPlugin, effects::liquids::LiquidsPlugin));
        app.add_plugins((post::fade_in::FadeInPlugin, post::photo::PhotoPlugin, effects::wet::WetPlugin, gpu::compiling::CompilingPipelinesPlugin, gpu::preprocess_reuse::PreprocessReusePlugin));
    }
}

pub struct RetailLightPlugin;

impl bevy::app::Plugin for RetailLightPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        // Our point and spot lights never cast Bevy shadow maps: the smallest cube map frees Bevy's 24 MB default.
        app.insert_resource(bevy::light::PointLightShadowMap { size: 1 });
        app.init_resource::<lighting::glow_lights::GlowLights>().init_resource::<lighting::bsp_light::FrameLights>().add_systems(
            bevy::app::Update,
            (lighting::glow_lights::light_glows, lighting::actor_light::light_actors, lighting::terrain_light::light_terrain, lighting::bsp_light::dynamic_lights)
                .chain()
                .in_set(world::view::SceneViewSystems::Light),
        );
    }
}
