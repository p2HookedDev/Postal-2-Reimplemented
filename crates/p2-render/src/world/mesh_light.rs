use bevy::app::{App, Plugin};
use bevy::asset::Handle;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, StandardMaterial};
use bevy::render::render_resource::{RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::world::shader_layers::ShaderLayers;
use crate::gpu::shaders::add_material;
use crate::gpu::shaders::SkyKey;

pub type MeshLitMaterial = ExtendedMaterial<StandardMaterial, DynamicLit>;

crate::world::shader_layers::layered_extension!(
    DynamicLit
);

const MESH_LIGHT_SHADER: Handle<Shader> = crate::gpu::shaders::shader_handle("mesh_light.wesl");

impl MaterialExtension for DynamicLit {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::gpu::shaders::unmark_normal_map(descriptor);
        crate::gpu::shaders::cast_both_sides(descriptor, key.mesh_key);
        crate::gpu::shaders::mark_see_through(descriptor, key.mesh_key);
        crate::gpu::shaders::mark_pool_mirror(descriptor, key.mesh_key, key.bind_group_data);
        crate::gpu::shaders::mark_glass_mirror(descriptor, key.mesh_key, key.bind_group_data);
        crate::gpu::shaders::test_depth_early(descriptor, key.mesh_key, key.bind_group_data);
        crate::gpu::shaders::mark_screen_sun(descriptor, key.mesh_key, key.bind_group_data);
        crate::post::reflections::write_class_in_prepass(descriptor, crate::post::reflections::LayersHeld::Alone);
        crate::gpu::shaders::mark_sky(descriptor, key.bind_group_data);
        Ok(())
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(MESH_LIGHT_SHADER)
    }
}

pub struct MeshLightPlugin;

impl Plugin for MeshLightPlugin {
    fn build(&self, app: &mut App) {
        add_material::<MeshLitMaterial>(app, "mesh_light.wesl");
    }
}
