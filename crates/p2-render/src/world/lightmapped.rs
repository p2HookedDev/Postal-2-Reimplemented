use bevy::app::{App, Plugin};
use bevy::asset::Handle;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, StandardMaterial};
use bevy::render::render_resource::{RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderDefVal, ShaderRef};

use crate::world::shader_layers::ShaderLayers;
use crate::gpu::shaders::add_material;
use crate::gpu::shaders::{LightSource, SkyKey};

pub type LightmappedMaterial = ExtendedMaterial<StandardMaterial, BakedLight>;

crate::world::shader_layers::layered_extension!(
    BakedLight
);

const LIGHTMAPPED_SHADER: Handle<Shader> = crate::gpu::shaders::shader_handle("lightmapped.wesl");

impl MaterialExtension for BakedLight {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
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
        let modern = key.bind_group_data.light == LightSource::Modern;
        if modern {
            crate::gpu::shaders::mark_cached_lamps(descriptor, layout);
        }
        // The specular slot carries the cached lamp visibility, which Bevy would otherwise sample as reflectance.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.retain(|def| match def {
                ShaderDefVal::Bool(name, _) => name != "PBR_SPECULAR_TEXTURES_SUPPORTED" && !(modern && (name == "LIGHTMAP" || name == "MULTIPLE_LIGHTMAPS_IN_ARRAY")),
                _ => true,
            });
        }
        Ok(())
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(LIGHTMAPPED_SHADER)
    }
}

pub struct LightmappedPlugin;

impl Plugin for LightmappedPlugin {
    fn build(&self, app: &mut App) {
        add_material::<LightmappedMaterial>(app, "lightmapped.wesl");
    }
}
