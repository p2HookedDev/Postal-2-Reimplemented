//! Alpha-blended surfaces: Bevy's `apply_decals` multiplies the colour by the surface's alpha, which
//! the blend applies again and darkens decal boxes, so [`BlendedMaterial`] runs decals at alpha 1.

use bevy::app::{App, Plugin};
use bevy::asset::{Asset, AssetApp, Handle, uuid_handle};
use bevy::image::Image;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline, MaterialPlugin, StandardMaterial};
use bevy::reflect::Reflect;
use bevy::render::RenderApp;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::render::shader_layers::{Layers, ShaderLayers};
use crate::render::shaders::add_shaders;
use crate::render::terrain_material::{TERRAIN_SHADER, TerrainMaterial};

pub type BlendedMaterial = ExtendedMaterial<StandardMaterial, OpaqueDecals>;

crate::render::shader_layers::layered_extension!(
    /// Bindless because Bevy keeps an extended material bindless only if both halves are.
    OpaqueDecals
);

/// The engine draws terrain with the world, before any translucent actor, but Bevy sorted blended layers among the
/// particles by their meshes' centres, so a layer covered fire behind it (particles write no depth). Bevy also takes
/// `depth_bias` as the pipeline's constant depth bias; [`OpaqueDecals::specialize`] takes this offset back out of it.
pub const TERRAIN_LAYER_SORT: f32 = -1_000_000.0;

/// Retail draws a translucent actor in the eye's leaf last; Bevy's box-centre sort flips it as the view turns.
pub const AROUND_EYE_SORT: f32 = 1_000_000.0;

const BLENDED_SHADER: Handle<Shader> = uuid_handle!("3b8e51a4-7c2d-4f69-b0e8-91d5a6c4f207");

impl MaterialExtension for OpaqueDecals {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(BLENDED_SHADER)
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        without_sort_offset(descriptor);
        Ok(())
    }
}

pub(crate) fn without_sort_offset(descriptor: &mut RenderPipelineDescriptor) {
    let offset = AROUND_EYE_SORT as i32;
    if let Some(depth) = descriptor.depth_stencil.as_mut() {
        if depth.bias.constant <= -offset / 2 {
            depth.bias.constant += offset;
        } else if depth.bias.constant >= offset / 2 {
            depth.bias.constant -= offset;
        }
    }
}

pub type TerrainLayerMaterial = ExtendedMaterial<StandardMaterial, TerrainLayer>;

/// Alpha map sampled per pixel at `UV_1`; vertex colour is baked light (display space 0..1), applied
/// as min(texture × 2 × light, 1). Alpha map bindings 109/110; index entries 107/108 are unused.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
#[data(100, ShaderLayers, binding_array(107))]
#[bindless(index_table(range(100..111), binding(108)))]
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
}

impl TerrainLayer {
    pub fn new(alpha_map: Handle<Image>, layers: &Layers) -> Self {
        let [detail, specular, flat_specular] = layers.images();
        Self { layers: layers.params, detail, specular, flat_specular, alpha_map: Some(alpha_map) }
    }
}

impl From<&TerrainLayer> for ShaderLayers {
    fn from(extension: &TerrainLayer) -> Self {
        extension.layers
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
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        without_sort_offset(descriptor);
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push("TERRAIN_LAYER".into());
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
            return;
        }
        crate::render::shaders::add_libraries(app);
        let terrain = Shader::from_wesl(include_str!("terrain.wesl"), "p2_engine/terrain.wesl");
        let blended = Shader::from_wesl(include_str!("blended.wesl"), "p2_engine/blended.wesl");
        let layer = Shader::from_wesl(include_str!("terrain_layer.wesl"), "p2_engine/terrain_layer.wesl");
        add_shaders(app, [(TERRAIN_SHADER, terrain), (BLENDED_SHADER, blended), (TERRAIN_LAYER_SHADER, layer)]);
        app.add_plugins((
            MaterialPlugin::<TerrainMaterial>::default(),
            MaterialPlugin::<BlendedMaterial>::default(),
            MaterialPlugin::<TerrainLayerMaterial>::default(),
        ));
    }
}
