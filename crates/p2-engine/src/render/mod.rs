pub mod actor_light;
pub mod blended;
pub mod bsp_light;
pub mod gamma;
pub mod lamps;
pub mod lamp_fog;
pub mod lightmapped;
pub mod map_lights;
pub mod mesh_light;
pub mod modern;
pub mod material_class;
pub mod lamp_size;
pub mod particle_material;
pub mod probes;
pub(crate) mod sampler_refresh;
pub mod shader_layers;
pub mod tone;
pub mod sun_cache;
pub mod translucent_order;
pub mod world_pass;
pub(crate) mod shaders;
pub(crate) mod terrain_light;
pub(crate) mod terrain_material;
pub(crate) mod wet;
pub(crate) mod wounds;
pub mod cutscene_lens;
pub mod modern_view;
pub mod reflections;
pub mod screen_pass;
pub mod bounce;
pub mod fire_volume;
pub mod temporal_aa;
pub mod fade_in;

pub struct RetailRenderPlugin;

impl bevy::app::Plugin for RetailRenderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.add_plugins((
            modern_view::ModernViewPlugin,
            cutscene_lens::CutsceneLensPlugin,
            reflections::ReflectionsPlugin,
            lamp_fog::LampFogPlugin,
            bounce::BounceLightPlugin,
            fire_volume::FireVolumePlugin,
            temporal_aa::TemporalAaPlugin,
            particle_material::ParticlePlugin,
            blended::BlendedPlugin,
            translucent_order::TranslucentOrderPlugin,
            lightmapped::LightmappedPlugin,
            actor_light::ActorLightPlugin,
            mesh_light::MeshLightPlugin,
            world_pass::WorldPassPlugin,
            sampler_refresh::SamplerRefreshPlugin,
        ));
        app.add_plugins(fade_in::FadeInPlugin);
    }
}

pub struct RetailLightPlugin;

impl bevy::app::Plugin for RetailLightPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        app.add_systems(
            bevy::app::Update,
            (actor_light::light_actors, terrain_light::light_terrain, bsp_light::dynamic_lights)
                .chain()
                .in_set(crate::mesh_view::SceneViewSystems::Light),
        );
    }
}
