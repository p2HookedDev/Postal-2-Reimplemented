pub mod actor_light;
pub mod blended;
pub mod bsp_light;
pub mod gamma;
pub mod lightmapped;
pub mod mesh_light;
pub mod modern;
pub mod particle_material;
pub(crate) mod sampler_refresh;
pub mod shader_layers;
pub mod translucent_order;
pub mod world_pass;
pub(crate) mod shaders;
pub(crate) mod sun_mask;
pub(crate) mod terrain_light;
pub(crate) mod terrain_material;
pub(crate) mod wet;

pub struct RetailRenderPlugin;

impl bevy::app::Plugin for RetailRenderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.add_plugins((
            particle_material::ParticlePlugin,
            blended::BlendedPlugin,
            translucent_order::TranslucentOrderPlugin,
            lightmapped::LightmappedPlugin,
            actor_light::ActorLightPlugin,
            mesh_light::MeshLightPlugin,
            world_pass::WorldPassPlugin,
            sampler_refresh::SamplerRefreshPlugin,
        ));
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
