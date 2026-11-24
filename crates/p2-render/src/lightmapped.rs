//! Light-mapped BSP drawn as the engine does. Bevy applies a `Lightmap` only in its lit path, so
//! [`LightmappedMaterial`] keeps Bevy's material input and skips the PBR lighting.

use bevy::app::{App, Plugin};
use bevy::asset::{Handle, uuid_handle};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, StandardMaterial};
use bevy::render::render_resource::{RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::shader_layers::ShaderLayers;
use crate::shaders::add_material;
use crate::shaders::SkyKey;

pub type LightmappedMaterial = ExtendedMaterial<StandardMaterial, BakedLight>;

crate::shader_layers::layered_extension!(
    BakedLight
);

const LIGHTMAPPED_SHADER: Handle<Shader> = uuid_handle!("9c4e7a12-5f3b-4d8e-a6c1-2b7f90e3d518");

impl MaterialExtension for BakedLight {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::shaders::cast_both_sides(descriptor, key.mesh_key);
        crate::shaders::mark_see_through(descriptor, key.mesh_key);
        crate::reflections::write_class_in_prepass(descriptor, crate::reflections::LayersHeld::Alone);
        crate::shaders::mark_sky(descriptor, key.bind_group_data);
        Ok(())
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(LIGHTMAPPED_SHADER)
    }
}

pub struct LightmappedPlugin;

impl Plugin for LightmappedPlugin {
    fn build(&self, app: &mut App) {
        add_material::<LightmappedMaterial>(app, LIGHTMAPPED_SHADER, "lightmapped.wesl");
    }
}
