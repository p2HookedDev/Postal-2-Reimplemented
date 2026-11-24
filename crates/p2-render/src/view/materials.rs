use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SceneLights {
    Used,
    Ignored,
}

fn actor_standard(section: &SectionView, lights: SceneLights) -> StandardMaterial {
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

pub fn alpha_key(mode: AlphaMode) -> (std::mem::Discriminant<AlphaMode>, u32) {
    (std::mem::discriminant(&mode), if let AlphaMode::Mask(cutoff) = mode { cutoff.to_bits() } else { 0 })
}

pub fn cull_for(two_sided: bool) -> Option<Face> {
    if two_sided { None } else { Some(Face::Back) }
}

pub(crate) fn image_size(images: &Assets<Image>, image: impl Into<bevy::asset::AssetId<Image>>) -> [f32; 2] {
    images.get(image).map_or([1.0, 1.0], |image| image.size().as_vec2().to_array())
}

#[derive(PartialEq, Eq, Hash)]
struct ActorMaterialKey {
    pub(super) texture: Option<bevy::asset::AssetId<Image>>,
    pub(super) layers: String,
    pub(super) alpha: (std::mem::Discriminant<AlphaMode>, u32),
    pub(super) no_scene_lights: bool,
    pub(super) two_sided: bool,
    pub(super) tint: [u32; 4],
    vertex_lit: bool,
}

/// Sections of one look share a material: of the actors spawned while a map plays until the map is left, of the scene's
/// own actors and attachments until the scene changes.
#[derive(Resource, Default)]
pub struct ActorMaterialCache {
    spawned: FxHashMap<ActorMaterialKey, SectionMaterial>,
    followers: FxHashMap<ActorMaterialKey, SectionMaterial>,
}

#[derive(Clone, Copy)]
pub enum ActorLook {
    Spawned,
    Canvas,
    Follower,
    Fluid,
}

#[derive(SystemParam)]
pub struct ActorMaterials<'w> {
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub(crate) mesh_lit: ResMut<'w, Assets<MeshLitMaterial>>,
    cache: ResMut<'w, ActorMaterialCache>,
}

impl ActorMaterials<'_> {
    pub fn material(&mut self, look: ActorLook, section: &SectionView) -> SectionMaterial {
        match look {
            ActorLook::Spawned => self.cached(look, section, || actor_standard(section, SceneLights::Ignored)),
            ActorLook::Canvas => SectionMaterial::Standard(self.materials.add(actor_standard(section, SceneLights::Ignored))),
            ActorLook::Follower => self.cached(look, section, || StandardMaterial { perceptual_roughness: 0.8, ..actor_standard(section, if section.no_scene_lights { SceneLights::Ignored } else { SceneLights::Used }) }),
            ActorLook::Fluid => section_material(section, actor_standard(section, SceneLights::Ignored), &mut self.materials, &mut self.mesh_lit),
        }
    }

    /// At a map change: the actors spawned while the map played are gone.
    pub fn forget_spawned(&mut self) {
        self.cache.spawned.clear();
    }

    pub(super) fn forget_followers(&mut self) {
        self.cache.followers.clear();
    }

    fn cached(&mut self, look: ActorLook, section: &SectionView, standard: impl FnOnce() -> StandardMaterial) -> SectionMaterial {
        let key = ActorMaterialKey {
            texture: section.texture.as_ref().map(Handle::id),
            layers: section.layers.key(),
            alpha: alpha_key(section.alpha_mode),
            no_scene_lights: section.no_scene_lights,
            two_sided: section.two_sided,
            tint: section.tint.map(f32::to_bits),
            vertex_lit: vertex_lit(section),
        };
        let Self { materials, mesh_lit, cache } = self;
        let shared = if matches!(look, ActorLook::Follower) { &mut cache.followers } else { &mut cache.spawned };
        shared.entry(key).or_insert_with(|| section_material(section, standard(), materials, mesh_lit)).clone()
    }

    pub(crate) fn set_uv(&mut self, material: &SectionMaterial, transform: Affine2) {
        match material {
            SectionMaterial::MeshLit(handle) => set_uv_transform(&mut self.mesh_lit, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::Standard(handle) => set_uv_transform(&mut self.materials, handle, transform, |m| m, |m| m),
            _ => {}
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum SectionMaterial {
    Standard(Handle<StandardMaterial>),
    SkyStandard(Handle<SkyStandardMaterial>),
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
            Self::SkyStandard(material) => material.id().untyped(),
            Self::Blended(material) => material.id().untyped(),
            Self::Terrain(material) => material.id().untyped(),
            Self::TerrainPass(material) => material.id().untyped(),
            Self::Lightmapped(material) => material.id().untyped(),
            Self::MeshLit(material) => material.id().untyped(),
        }
    }

    pub fn insert(&self, entity: &mut EntityCommands) {
        match self {
            Self::Standard(material) => entity.insert(MeshMaterial3d(material.clone())),
            Self::SkyStandard(material) => entity.insert(MeshMaterial3d(material.clone())),
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

fn section_material(
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
    pub(super) actors: ActorMaterials<'w>,
    pub(super) blended: ResMut<'w, Assets<BlendedMaterial>>,
    sky_standard: ResMut<'w, Assets<SkyStandardMaterial>>,
    pub(super) terrain_layers: ResMut<'w, Assets<TerrainLayerMaterial>>,
    terrain_passes: ResMut<'w, Assets<TerrainMaterial>>,
    pub(super) lightmapped: ResMut<'w, Assets<LightmappedMaterial>>,
    images: Res<'w, Assets<Image>>,
    pub(super) animated: ResMut<'w, AnimatedMaterials>,
}

pub(super) fn scene_section_material(
    section: &SectionView,
    center: Vec3,
    sky: Option<(Vec3, Quat)>,
    stores: &mut SectionMaterials,
    cache: &mut FxHashMap<(bool, bool, String), SectionMaterial>,
) -> SectionMaterial {
    let Some(texture) = &section.texture else { return SectionMaterial::Standard(stores.grey.0.clone()) };
    let vertex_lit = vertex_lit(section);
    let in_sky = sky.is_some();
    let key = (vertex_lit, in_sky, section.material_key());
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
        SectionMaterial::TerrainPass(stores.terrain_passes.add(TerrainMaterial { base, extension: sky.into_iter().fold(TerrainLayers::new(&section.terrain, center), TerrainLayers::in_sky) }))
    } else if let Some(alpha_map) = &section.alpha_map {
        let extension = TerrainLayer { sky: in_sky, ..TerrainLayer::new(alpha_map.clone(), &section.layers) };
        SectionMaterial::Terrain(stores.terrain_layers.add(TerrainLayerMaterial { base: section.layers.lit(standard), extension }))
    } else if section.lightmap.is_some() {
        SectionMaterial::Lightmapped(stores.lightmapped.add(LightmappedMaterial { base: section.layers.lit(standard), extension: BakedLight { sky: in_sky, ..BakedLight::new(&section.layers) } }))
    } else if vertex_lit {
        SectionMaterial::MeshLit(stores.actors.mesh_lit.add(MeshLitMaterial { base: section.layers.lit(standard), extension: DynamicLit { sky: in_sky, ..DynamicLit::new(&section.layers) } }))
    } else if section.alpha_mode == AlphaMode::Blend && section.uv.is_empty() {
        SectionMaterial::Blended(stores.blended.add(BlendedMaterial { base: section.layers.lit(standard), extension: OpaqueDecals { sky: in_sky, ..OpaqueDecals::new(&section.layers) } }))
    } else if in_sky {
        SectionMaterial::SkyStandard(stores.sky_standard.add(SkyStandardMaterial { base: standard, extension: SkyStandard {} }))
    } else {
        SectionMaterial::Standard(stores.actors.materials.add(standard))
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
