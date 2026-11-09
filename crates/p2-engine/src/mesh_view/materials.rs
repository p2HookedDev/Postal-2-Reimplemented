use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SceneLights {
    Used,
    Ignored,
}

pub(crate) fn actor_standard(section: &SectionView, lights: SceneLights) -> StandardMaterial {
    let [r, g, b, a] = section.tint;
    StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_texture: section.texture.clone(),
        emissive: section.emissive(),
        emissive_texture: section.glow.clone(),
        alpha_mode: section.alpha_mode,
        unlit: lights == SceneLights::Ignored,
        cull_mode: cull_for(section.two_sided),
        double_sided: section.two_sided,
        ..Default::default()
    }
}

pub(crate) fn alpha_key(mode: AlphaMode) -> (std::mem::Discriminant<AlphaMode>, u32) {
    (std::mem::discriminant(&mode), if let AlphaMode::Mask(cutoff) = mode { cutoff.to_bits() } else { 0 })
}

pub(crate) fn cull_for(two_sided: bool) -> Option<Face> {
    if two_sided { None } else { Some(Face::Back) }
}

pub(crate) fn image_size(images: &Assets<Image>, image: impl Into<bevy::asset::AssetId<Image>>) -> [f32; 2] {
    images.get(image).map_or([1.0, 1.0], |image| image.size().as_vec2().to_array())
}

#[derive(PartialEq, Eq, Hash)]
pub struct ActorMaterialKey {
    pub(super) texture: Option<bevy::asset::AssetId<Image>>,
    pub(super) layers: String,
    pub(super) alpha: (std::mem::Discriminant<AlphaMode>, u32),
    pub(super) no_scene_lights: bool,
    pub(super) two_sided: bool,
    pub(super) tint: [u32; 4],
    vertex_lit: bool,
}

pub(crate) fn cached_actor_material(
    cache: &mut FxHashMap<ActorMaterialKey, SectionMaterial>,
    section: &SectionView,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
    standard: impl FnOnce() -> StandardMaterial,
) -> SectionMaterial {
    let key = ActorMaterialKey {
        texture: section.texture.as_ref().map(Handle::id),
        layers: section.layers.key(),
        alpha: alpha_key(section.alpha_mode),
        no_scene_lights: section.no_scene_lights,
        two_sided: section.two_sided,
        tint: section.tint.map(f32::to_bits),
        vertex_lit: vertex_lit(section),
    };
    cache.entry(key).or_insert_with(|| section_material(section, standard(), materials, mesh_lit)).clone()
}

pub fn follower_material(
    section: &SectionView,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
    cache: &mut FxHashMap<ActorMaterialKey, SectionMaterial>,
) -> SectionMaterial {
    cached_actor_material(cache, section, materials, mesh_lit, || StandardMaterial { perceptual_roughness: 0.8, ..actor_standard(section, if section.no_scene_lights { SceneLights::Ignored } else { SceneLights::Used }) })
}

#[derive(Clone, PartialEq)]
pub enum SectionMaterial {
    Standard(Handle<StandardMaterial>),
    Blended(Handle<BlendedMaterial>),
    Terrain(Handle<TerrainLayerMaterial>),
    TerrainPass(Handle<TerrainMaterial>),
    Lightmapped(Handle<LightmappedMaterial>),
    MeshLit(Handle<MeshLitMaterial>),
}

impl SectionMaterial {
    pub(crate) fn id(&self) -> UntypedAssetId {
        match self {
            Self::Standard(material) => material.id().untyped(),
            Self::Blended(material) => material.id().untyped(),
            Self::Terrain(material) => material.id().untyped(),
            Self::TerrainPass(material) => material.id().untyped(),
            Self::Lightmapped(material) => material.id().untyped(),
            Self::MeshLit(material) => material.id().untyped(),
        }
    }

    pub(crate) fn insert(&self, entity: &mut EntityCommands) {
        match self {
            Self::Standard(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::Blended(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::Terrain(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::TerrainPass(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::Lightmapped(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::MeshLit(material) => entity.insert(MeshMaterial3d(material.clone())),
        };
    }
}

pub fn vertex_lit(section: &SectionView) -> bool {
    section.no_scene_lights && !section.fullbright && section.skin.is_none()
}

pub fn section_material(
    section: &SectionView,
    standard: StandardMaterial,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
) -> SectionMaterial {
    if vertex_lit(section) {
        SectionMaterial::MeshLit(mesh_lit.add(MeshLitMaterial { base: section.layers.lit(standard), extension: DynamicLit::new(&section.layers) }))
    } else {
        SectionMaterial::Standard(materials.add(standard))
    }
}

#[derive(SystemParam)]
pub(super) struct SectionMaterials<'w> {
    grey: Res<'w, MissingTextureMaterial>,
    pub(super) materials: ResMut<'w, Assets<StandardMaterial>>,
    pub(super) blended: ResMut<'w, Assets<BlendedMaterial>>,
    pub(super) terrain_layers: ResMut<'w, Assets<TerrainLayerMaterial>>,
    terrain_passes: ResMut<'w, Assets<TerrainMaterial>>,
    pub(super) lightmapped: ResMut<'w, Assets<LightmappedMaterial>>,
    pub(super) mesh_lit: ResMut<'w, Assets<MeshLitMaterial>>,
    images: Res<'w, Assets<Image>>,
    pub(super) animated: ResMut<'w, AnimatedMaterials>,
}

pub(super) fn scene_section_material(section: &SectionView, center: Vec3, stores: &mut SectionMaterials, cache: &mut FxHashMap<(bool, String), SectionMaterial>) -> SectionMaterial {
    let Some(texture) = &section.texture else { return SectionMaterial::Standard(stores.grey.0.clone()) };
    let vertex_lit = vertex_lit(section);
    let key = (vertex_lit, section.material_key());
    if let Some(material) = cache.get(&key) {
        return material.clone();
    }
    let [r, g, b, a] = section.tint;
    let standard = StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_texture: Some(texture.clone()),
        emissive: section.emissive(),
        emissive_texture: section.glow.clone(),
        alpha_mode: section.alpha_mode,
        perceptual_roughness: 0.8,
        unlit: section.no_scene_lights && section.lightmap.is_none(),
        reflectance: if section.lightmap.is_some() { 0.0 } else { 0.5 },
        lightmap_exposure: MODULATE_2X / Exposure::default().exposure(),
        cull_mode: cull_for(section.two_sided),
        double_sided: section.two_sided,
        ..Default::default()
    };
    let material = if !section.terrain.is_empty() {
        let base = StandardMaterial { base_color_texture: None, ..standard };
        SectionMaterial::TerrainPass(stores.terrain_passes.add(TerrainMaterial { base, extension: TerrainLayers::new(&section.terrain, center) }))
    } else if let Some(alpha_map) = &section.alpha_map {
        let extension = TerrainLayer::new(alpha_map.clone(), &section.layers);
        SectionMaterial::Terrain(stores.terrain_layers.add(TerrainLayerMaterial { base: section.layers.lit(standard), extension }))
    } else if section.lightmap.is_some() {
        SectionMaterial::Lightmapped(stores.lightmapped.add(LightmappedMaterial { base: section.layers.lit(standard), extension: BakedLight::new(&section.layers) }))
    } else if vertex_lit {
        SectionMaterial::MeshLit(stores.mesh_lit.add(MeshLitMaterial { base: section.layers.lit(standard), extension: DynamicLit::new(&section.layers) }))
    } else if section.alpha_mode == AlphaMode::Blend && section.uv.is_empty() {
        SectionMaterial::Blended(stores.blended.add(BlendedMaterial { base: section.layers.lit(standard), extension: OpaqueDecals::new(&section.layers) }))
    } else {
        SectionMaterial::Standard(stores.materials.add(standard))
    };
    let size = |image: &Handle<Image>| image_size(&stores.images, image);
    if !section.uv.is_empty() {
        let moving = MovingUv { material: material.clone(), modifiers: section.uv.clone(), size: size(texture) };
        stores.animated.textures.push(moving);
    }
    if let Some(specular) = section.layers.flat_specular.as_ref().filter(|_| !section.layers.specular_uv.is_empty()) {
        let moving = MovingUv { material: material.clone(), modifiers: section.layers.specular_uv.clone(), size: size(specular) };
        stores.animated.speculars.push(moving);
    }
    cache.insert(key, material.clone());
    material
}
