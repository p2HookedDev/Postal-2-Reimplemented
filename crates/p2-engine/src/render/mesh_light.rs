use bevy::app::{App, Plugin};
use bevy::asset::{AssetApp, Handle, uuid_handle};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin, StandardMaterial};
use bevy::render::RenderApp;
use bevy::shader::{Shader, ShaderRef};

use crate::render::shader_layers::ShaderLayers;
use crate::render::shaders::{add_libraries, add_shaders};

pub type MeshLitMaterial = ExtendedMaterial<StandardMaterial, DynamicLit>;

crate::render::shader_layers::layered_extension!(
    DynamicLit
);

const MESH_LIGHT_SHADER: Handle<Shader> = uuid_handle!("3b8d51f6-0c2e-4a97-b4d3-7e15a9c2f860");

impl MaterialExtension for DynamicLit {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(MESH_LIGHT_SHADER)
    }
}

pub struct MeshLightPlugin;

impl Plugin for MeshLightPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            app.init_asset::<MeshLitMaterial>();
            return;
        }
        add_libraries(app);
        let shader = Shader::from_wesl(include_str!("mesh_light.wesl"), "p2_engine/mesh_light.wesl");
        add_shaders(app, [(MESH_LIGHT_SHADER, shader)]);
        app.add_plugins(MaterialPlugin::<MeshLitMaterial>::default());
    }
}
