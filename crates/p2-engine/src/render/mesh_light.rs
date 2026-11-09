use bevy::app::{App, Plugin};
use bevy::asset::{Handle, uuid_handle};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, StandardMaterial};
use bevy::render::render_resource::{RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::render::shader_layers::ShaderLayers;
use crate::render::shaders::add_material;

pub type MeshLitMaterial = ExtendedMaterial<StandardMaterial, DynamicLit>;

crate::render::shader_layers::layered_extension!(
    DynamicLit
);

const MESH_LIGHT_SHADER: Handle<Shader> = uuid_handle!("3b8d51f6-0c2e-4a97-b4d3-7e15a9c2f860");

impl MaterialExtension for DynamicLit {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(MESH_LIGHT_SHADER)
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::render::blended::without_sort_offset(descriptor);
        Ok(())
    }
}

pub struct MeshLightPlugin;

impl Plugin for MeshLightPlugin {
    fn build(&self, app: &mut App) {
        add_material::<MeshLitMaterial>(app, MESH_LIGHT_SHADER, include_str!("mesh_light.wesl"), "p2_engine/mesh_light.wesl");
    }
}
