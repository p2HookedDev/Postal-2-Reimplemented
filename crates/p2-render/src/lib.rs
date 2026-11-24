pub mod components;
pub mod level;
pub mod level_collision;
pub mod actor_light;
pub mod asset_churn;
pub mod blended;
pub mod canvas;
pub mod compiling;
pub mod bsp_light;
pub mod gamma;
pub mod lamps;
pub(crate) mod lamp_fog;
pub mod lightmapped;
pub mod map_lights;
pub mod mesh_light;
pub mod modern;
pub mod modern_bloom;
pub mod material_class;
pub(crate) mod lamp_size;
pub(crate) mod overlay_route;
pub(crate) mod overlay_sun;
pub mod particle_draw;
pub mod particle_frame;
pub(crate) mod particle_throw;
pub mod particle_material;
pub mod probes;
pub(crate) mod preprocess_reuse;
pub(crate) mod sampler_refresh;
pub mod shader_layers;
pub(crate) mod tone;
pub mod sun_cache;
pub mod sun_levels;
pub mod translucent_order;
pub mod world_pass;
pub(crate) mod shaders;
pub(crate) mod slot_buffer;
pub(crate) mod terrain_light;
pub mod terrain_material;
pub mod wet;
pub mod wounds;
pub mod modern_view;
pub(crate) mod reflections;
pub(crate) mod screen_pass;
pub(crate) mod stage_encoder;
pub(crate) mod bounce;
pub mod coronas;
pub mod debris;
pub mod fire_volume;
pub mod gpu_particles;
pub mod liquids;
pub mod fluids;
pub(crate) mod temporal_aa;
pub(crate) mod fade_in;
pub mod frame_facts;
pub(crate) mod glow_lights;
pub mod photo;
pub mod scene;
pub mod sprites;
pub mod view;

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

/// What every host that draws a scene needs. `GammaPlugin` and `ModernGraphicsPlugin` are added beside it by a
/// host with the game's window and settings.
pub struct P2RenderPlugins;

impl bevy::app::PluginGroup for P2RenderPlugins {
    fn build(self) -> bevy::app::PluginGroupBuilder {
        bevy::app::PluginGroupBuilder::start::<Self>().add(RetailRenderPlugin).add(RetailLightPlugin).add(view::MeshViewPlugin)
    }
}

pub struct RetailRenderPlugin;

impl bevy::app::Plugin for RetailRenderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.init_resource::<frame_facts::FrameFacts>();
        app.add_plugins((
            modern_view::ModernViewPlugin,
            reflections::ReflectionsPlugin,
            lamp_fog::LampFogPlugin,
            (bounce::BounceLightPlugin, modern_bloom::ModernBloomPlugin),
            fire_volume::FireVolumePlugin,
            temporal_aa::TemporalAaPlugin,
            particle_material::ParticlePlugin,
            particle_draw::ParticleDrawPlugin,
            blended::BlendedPlugin,
            translucent_order::TranslucentOrderPlugin,
            lightmapped::LightmappedPlugin,
            actor_light::ActorLightPlugin,
            mesh_light::MeshLightPlugin,
            world_pass::WorldPassPlugin,
            sampler_refresh::SamplerRefreshPlugin,
        ));
        app.add_plugins((sprites::SpritePlugin, coronas::CoronaPlugin, fluids::FluidPlugin, gpu_particles::GpuParticlesPlugin, debris::DebrisPlugin, liquids::LiquidsPlugin));
        app.add_plugins((fade_in::FadeInPlugin, photo::PhotoPlugin, wet::WetPlugin, compiling::CompilingPipelinesPlugin, preprocess_reuse::PreprocessReusePlugin));
    }
}

pub struct RetailLightPlugin;

impl bevy::app::Plugin for RetailLightPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        app.init_resource::<glow_lights::GlowLights>().init_resource::<bsp_light::FrameLights>().add_systems(
            bevy::app::Update,
            (glow_lights::light_glows, actor_light::light_actors, terrain_light::light_terrain, bsp_light::dynamic_lights)
                .chain()
                .in_set(view::SceneViewSystems::Light),
        );
    }
}
