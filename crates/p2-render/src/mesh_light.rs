use bevy::app::{App, Plugin};
use bevy::asset::{Handle, uuid_handle};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, StandardMaterial};
use bevy::render::render_resource::{RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::shader_layers::ShaderLayers;
use crate::shaders::add_material;
use crate::shaders::SkyKey;

pub type MeshLitMaterial = ExtendedMaterial<StandardMaterial, DynamicLit>;

crate::shader_layers::layered_extension!(
    DynamicLit
);

const MESH_LIGHT_SHADER: Handle<Shader> = uuid_handle!("3b8d51f6-0c2e-4a97-b4d3-7e15a9c2f860");

impl MaterialExtension for DynamicLit {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::shaders::unmark_normal_map(descriptor);
        crate::shaders::cast_both_sides(descriptor, key.mesh_key);
        crate::shaders::mark_see_through(descriptor, key.mesh_key);
        crate::shaders::mark_pool_mirror(descriptor, key.mesh_key, key.bind_group_data);
        crate::shaders::test_depth_early(descriptor, key.mesh_key, key.bind_group_data);
        crate::shaders::mark_screen_sun(descriptor, key.mesh_key, key.bind_group_data);
        crate::reflections::write_class_in_prepass(descriptor, crate::reflections::LayersHeld::Alone);
        crate::shaders::mark_sky(descriptor, key.bind_group_data);
        Ok(())
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(MESH_LIGHT_SHADER)
    }
}

pub struct MeshLightPlugin;

impl Plugin for MeshLightPlugin {
    fn build(&self, app: &mut App) {
        add_material::<MeshLitMaterial>(app, MESH_LIGHT_SHADER, "mesh_light.wesl");
    }
}
