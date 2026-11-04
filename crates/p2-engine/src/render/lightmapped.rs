//! Light-mapped BSP drawn as the engine does. Bevy applies a `Lightmap` only in its lit path, so
//! [`LightmappedMaterial`] keeps Bevy's material input and skips the PBR lighting.

use bevy::app::{App, Plugin};
use bevy::asset::{Handle, uuid_handle};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::shader::{Shader, ShaderRef};

use crate::render::shader_layers::ShaderLayers;
use crate::render::shaders::add_material;

pub type LightmappedMaterial = ExtendedMaterial<StandardMaterial, BakedLight>;

crate::render::shader_layers::layered_extension!(
    BakedLight
);

const LIGHTMAPPED_SHADER: Handle<Shader> = uuid_handle!("9c4e7a12-5f3b-4d8e-a6c1-2b7f90e3d518");

impl MaterialExtension for BakedLight {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(LIGHTMAPPED_SHADER)
    }
}

pub struct LightmappedPlugin;

impl Plugin for LightmappedPlugin {
    fn build(&self, app: &mut App) {
        add_material::<LightmappedMaterial>(app, LIGHTMAPPED_SHADER, include_str!("lightmapped.wesl"), "p2_engine/lightmapped.wesl");
    }
}
