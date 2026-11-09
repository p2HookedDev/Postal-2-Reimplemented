mod actor;
mod convex_volume;
mod font;
mod karma;
mod karma_asset;
mod level;
mod material;
mod mesh_animation;
mod procedural;
mod model;
pub mod pose;
mod read;
mod skeletal_mesh;
mod sound;
mod static_mesh;
mod terrain;
mod terrain_deco;
mod texture;

pub use actor::{EDrawType, MeshActor,read_actor_location, read_actor_rotation, read_mesh_actor};
pub use convex_volume::{ConvexEdge, ConvexFace, ConvexVolume, read_convex_volume};
pub use font::{Font, FontCharacter, read_font};
pub use karma::{AggGeom, BoxElem, ConvexElem, CylinderElem, ElemTm, KMeshProps, SphereElem, read_kmesh_props};
pub use karma_asset::{KaConeType, KaGeometry, KaJoint, KaJointKind, KaMatrix, KaModel, KaPart, KaPrimitive, KaShape, KaTwistType, KarmaAsset, read_karma_assets};
pub use level::{Level, Url, read_level};
use material::MAX_MATERIAL_DEPTH;
pub use material::{BaseTexture, Blending, AlphaOperation, ColorOperation, CombineOps, DetailLayer, FrameBufferBlending, GlowLayer, OscillationType, SpecularLayer, Combiner, FinalBlend, RenderState, UvMatrix, UvModifier, base_texture, read_combiner, read_cubemap_faces, uv_matrix};
pub use model::{
    Aabb, BspNode, BspSection, BspSurface, BspVertex, Leaf, LightMap, LightMapTexture, Model, ModelPolygon, Poly, Vert,
    Zone, fan_triangles, poly_flags, read_model, read_polys,
};
pub use mesh_animation::{AnalogTrack, AnimNotify, AnimSequence, MeshAnimation, PoseAt, NamedBone, read_mesh_animation};
pub use procedural::{WaterDrop, WetParams, read_wet_params};
pub use skeletal_mesh::{AttachAlias, Bone, Influence, SkelMeshSection, SkeletalMesh, Wedge, read_primitive_sphere, read_skeletal_mesh};
pub use static_mesh::{
    MeshSection, StaticMesh, StaticMeshInstance, StaticMeshLight, read_static_mesh, read_static_mesh_instance, read_static_mesh_materials,
};
pub use sound::{Sound, read_sound, read_sound_duration, riff_chunks, sound_file_span, wave_duration_by};
pub use terrain::{Terrain, TerrainLayer, read_terrain};
pub use terrain_deco::{DecoLayer, DecoMaps, Decoration, TexelMap};
pub use texture::{LOD_SET_LIGHT_MAP, Mip, Texture, TextureFormat, read_palette, read_texture, mip_rgba, texture_alpha, texture_bgra, texture_heights, texture_rgba};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Package(#[from] p2_package::Error),
    #[error(transparent)]
    Script(#[from] p2_script::Error),
    #[error("unknown texture format {0}")]
    UnknownTextureFormat(u8),
    #[error("object reference {0} is out of range")]
    BadReference(i32),
    #[error("material chain from object {0} is longer than {MAX_MATERIAL_DEPTH} steps (cycle?)")]
    MaterialChain(i32),
    #[error("Karma asset: {0}")]
    KarmaAsset(String),
}
