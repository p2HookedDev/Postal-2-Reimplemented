//! Alpha-blended surfaces: Bevy's own fragment blends every clustered decal, projectors too, into the colour and
//! multiplies it by the surface's alpha, which the blend applies again; [`BlendedMaterial`] projects after the lighting
//! as the opaque surfaces do.

use bevy::app::{App, Plugin};
use bevy::asset::{Asset, AssetApp, Handle, uuid_handle};
use bevy::image::Image;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, MaterialPlugin, MeshPipelineKey, StandardMaterial};
use bevy::reflect::Reflect;
use bevy::render::RenderApp;
use bevy::render::render_resource::{AsBindGroup, BlendComponent, BlendFactor, BlendOperation, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::shader_layers::{Layers, ShaderLayers};
use crate::shaders::{SkyKey, add_shaders, shader_file};
use crate::terrain_material::{TERRAIN_SHADER, TerrainMaterial};

pub type BlendedMaterial = ExtendedMaterial<StandardMaterial, OpaqueDecals>;

crate::shader_layers::layered_extension!(
    /// Bindless because Bevy keeps an extended material bindless only if both halves are.
    OpaqueDecals
);

const BLENDED_SHADER: Handle<Shader> = uuid_handle!("3b8e51a4-7c2d-4f69-b0e8-91d5a6c4f207");

impl MaterialExtension for OpaqueDecals {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(BLENDED_SHADER)
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::shaders::unmark_normal_map(descriptor);
        crate::shaders::mark_sky(descriptor, key.bind_group_data);
        Ok(())
    }
}

/// A sky section Bevy's standard material draws: its own look, the sky's vertex stage.
pub type SkyStandardMaterial = ExtendedMaterial<StandardMaterial, SkyStandard>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct SkyStandard {}

impl MaterialExtension for SkyStandard {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::shaders::sky_vertex(descriptor);
        Ok(())
    }
}

pub type TerrainLayerMaterial = ExtendedMaterial<StandardMaterial, TerrainLayer>;

/// Alpha map sampled per pixel at `UV_1`; vertex colour is baked light (display space 0..1), applied
/// as min(texture × 2 × light, 1). Alpha map bindings 109/110; index entries 107/108 are unused.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
#[data(100, ShaderLayers, binding_array(107))]
#[bindless(index_table(range(100..111), binding(108)))]
#[bind_group_data(SkyKey)]
pub struct TerrainLayer {
    pub layers: ShaderLayers,
    #[texture(101)]
    #[sampler(102)]
    pub detail: Option<Handle<Image>>,
    #[texture(103, dimension = "cube")]
    #[sampler(104)]
    pub specular: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    pub flat_specular: Option<Handle<Image>>,
    #[texture(109)]
    #[sampler(110)]
    pub alpha_map: Option<Handle<Image>>,
    pub sky: bool,
}

impl TerrainLayer {
    pub fn new(alpha_map: Handle<Image>, layers: &Layers) -> Self {
        let [detail, specular, flat_specular] = layers.images();
        Self { layers: layers.params, detail, specular, flat_specular, alpha_map: Some(alpha_map), sky: false }
    }
}

impl From<&TerrainLayer> for ShaderLayers {
    fn from(extension: &TerrainLayer) -> Self {
        extension.layers
    }
}

impl From<&TerrainLayer> for SkyKey {
    fn from(extension: &TerrainLayer) -> Self {
        Self { sky: extension.sky, mirror: crate::modern::pool_mirror_on(extension.layers.detail.z as u32) }
    }
}

const TERRAIN_LAYER_SHADER: Handle<Shader> = uuid_handle!("5d27c8e1-94af-4b3e-8a6d-0f1e72b9c463");

impl MaterialExtension for TerrainLayer {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(TERRAIN_LAYER_SHADER)
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        crate::shaders::unmark_normal_map(descriptor);
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push("TERRAIN_LAYER".into());
        }
        crate::shaders::mark_pool_mirror(descriptor, key.mesh_key, key.bind_group_data);
        crate::shaders::mark_sky(descriptor, key.bind_group_data);
        // A layer over the base lays colour only: the base under it wrote the same pool's record into the alpha.
        if key.bind_group_data.mirror
            && key.mesh_key.intersection(MeshPipelineKey::BLEND_RESERVED_BITS) == MeshPipelineKey::BLEND_ALPHA
            && let Some(blend) = descriptor.fragment.as_mut().and_then(|fragment| fragment.targets.first_mut()).and_then(|target| target.as_mut()).and_then(|target| target.blend.as_mut())
        {
            blend.alpha = BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add };
        }
        Ok(())
    }
}

pub struct BlendedPlugin;

impl Plugin for BlendedPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            app.init_asset::<BlendedMaterial>();
            app.init_asset::<TerrainLayerMaterial>();
            app.init_asset::<TerrainMaterial>();
            app.init_asset::<SkyStandardMaterial>();
            return;
        }
        crate::shaders::add_libraries(app);
        let terrain = shader_file("terrain.wesl");
        let blended = shader_file("blended.wesl");
        let layer = shader_file("terrain_layer.wesl");
        add_shaders(app, [(TERRAIN_SHADER, terrain), (BLENDED_SHADER, blended), (TERRAIN_LAYER_SHADER, layer)]);
        app.add_plugins((
            MaterialPlugin::<TerrainMaterial>::default(),
            MaterialPlugin::<BlendedMaterial>::default(),
            MaterialPlugin::<TerrainLayerMaterial>::default(),
            MaterialPlugin::<SkyStandardMaterial>::default(),
        ));
    }
}
