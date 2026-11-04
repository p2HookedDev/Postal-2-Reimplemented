use bevy::app::{App, Plugin};
use bevy::asset::{AssetApp, Assets, Handle, uuid_handle};
use bevy::pbr::{Material, MaterialPlugin};
use bevy::render::RenderApp;
use bevy::shader::Shader;

pub(crate) fn add_shaders(app: &mut App, shaders: impl IntoIterator<Item = (Handle<Shader>, Shader)>) {
    let mut assets = app.world_mut().resource_mut::<Assets<Shader>>();
    for (handle, shader) in shaders {
        let path = shader.path.clone();
        if let Err(error) = assets.insert(handle.id(), shader) {
            bevy::log::error!("shader {path}: {error}");
        }
    }
}

const DYNAMIC_LIGHT_SHADER: Handle<Shader> = uuid_handle!("e27a4c90-6f1d-4b58-9a3e-d05c8b71f4a2");
const SHADER_LAYERS_SHADER: Handle<Shader> = uuid_handle!("8f4b2d61-93a7-4c0e-b5d8-1a6e7c39f204");
const LAYER_BINDINGS_SHADER: Handle<Shader> = uuid_handle!("c61e0a3f-7d52-4b9e-8f14-3a90d7e2b5c8");
const MODERN_SHADER: Handle<Shader> = uuid_handle!("4e9b7c21-0a6f-4d3e-b852-91c7f0e3a6d4");
const COLOR_SHADER: Handle<Shader> = uuid_handle!("9d3c6a57-2e1b-4f80-a7c4-5b0e8d91f236");
const PROJECTOR_SHADER: Handle<Shader> = uuid_handle!("b7e2f05c-8a31-4d96-9c4b-2f6d1e8a7c53");

pub(crate) struct ShaderLibraryPlugin;

impl Plugin for ShaderLibraryPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [
            (DYNAMIC_LIGHT_SHADER, Shader::from_wesl(include_str!("dynamic_light.wesl"), "p2_engine/dynamic_light.wesl")),
            (SHADER_LAYERS_SHADER, Shader::from_wesl(include_str!("shader_layers.wesl"), "p2_engine/shader_layers.wesl")),
            (LAYER_BINDINGS_SHADER, Shader::from_wesl(include_str!("layer_bindings.wesl"), "p2_engine/layer_bindings.wesl")),
            (MODERN_SHADER, Shader::from_wesl(include_str!("modern.wesl"), "p2_engine/modern.wesl")),
            (COLOR_SHADER, Shader::from_wesl(include_str!("color.wesl"), "p2_engine/color.wesl")),
            (PROJECTOR_SHADER, Shader::from_wesl(include_str!("projector.wesl"), "p2_engine/projector.wesl")),
        ]);
    }
}

pub(crate) fn add_libraries(app: &mut App) {
    if !app.is_plugin_added::<ShaderLibraryPlugin>() {
        app.add_plugins(ShaderLibraryPlugin);
    }
}

pub(crate) fn add_material<M: Material>(app: &mut App, handle: Handle<Shader>, source: &'static str, path: &'static str)
where
    MaterialPlugin<M>: Plugin,
{
    if app.get_sub_app(RenderApp).is_none() {
        app.init_asset::<M>();
        return;
    }
    add_libraries(app);
    add_shaders(app, [(handle, Shader::from_wesl(source, path))]);
    app.add_plugins(MaterialPlugin::<M>::default());
}
