use super::*;

#[derive(Resource, Default)]
pub(super) struct AnimatedMaterials {
    pub(super) textures: Vec<MovingUv>,
    pub(super) speculars: Vec<MovingUv>,
}

pub(super) struct MovingUv {
    pub(super) material: SectionMaterial,
    pub(super) modifiers: Vec<UvModifier>,
    pub(super) size: [f32; 2],
}

pub(crate) fn set_uv_transform<M: bevy::asset::Asset>(
    store: &mut Assets<M>,
    handle: &Handle<M>,
    transform: Affine2,
    base: impl Fn(&M) -> &StandardMaterial,
    base_mut: impl Fn(&mut M) -> &mut StandardMaterial,
) {
    if store.get(handle).is_some_and(|material| base(material).uv_transform != transform)
        && let Some(mut material) = store.get_mut(handle)
    {
        base_mut(&mut material).uv_transform = transform;
    }
}

pub(crate) fn set_specular_uv<M: bevy::asset::Asset>(
    store: &mut Assets<M>,
    handle: &Handle<M>,
    uv: (Vec4, Vec4),
    layers: impl Fn(&M) -> &ShaderLayers,
    layers_mut: impl Fn(&mut M) -> &mut ShaderLayers,
) {
    if store.get(handle).is_some_and(|material| (layers(material).specular_u, layers(material).specular_v) != uv)
        && let Some(mut material) = store.get_mut(handle)
    {
        let layers = layers_mut(&mut material);
        (layers.specular_u, layers.specular_v) = uv;
    }
}

pub(super) fn animate_materials(
    time: Res<Time>,
    animated: Res<AnimatedMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut lightmapped: ResMut<Assets<LightmappedMaterial>>,
    mut mesh_lit: ResMut<Assets<MeshLitMaterial>>,
    mut blended: ResMut<Assets<BlendedMaterial>>,
    mut terrain_layers: ResMut<Assets<TerrainLayerMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for MovingUv { material, modifiers, size } in &animated.textures {
        let [[a, b, c], [d, e, f]] = uv_matrix(modifiers, seconds, *size);
        let transform = Affine2::from_cols(Vec2::new(a, d), Vec2::new(b, e), Vec2::new(c, f));
        match material {
            SectionMaterial::Standard(handle) => set_uv_transform(&mut materials, handle, transform, |m| m, |m| m),
            SectionMaterial::Lightmapped(handle) => set_uv_transform(&mut lightmapped, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::MeshLit(handle) => set_uv_transform(&mut mesh_lit, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::Blended(handle) => set_uv_transform(&mut blended, handle, transform, |m| &m.base, |m| &mut m.base),
            SectionMaterial::Terrain(_) | SectionMaterial::TerrainPass(_) => {}
        }
    }
    for MovingUv { material, modifiers, size } in &animated.speculars {
        let [[a, b, c], [d, e, f]] = uv_matrix(modifiers, seconds, *size);
        let uv = (Vec4::new(a, b, c, 0.0), Vec4::new(d, e, f, 0.0));
        match material {
            SectionMaterial::Lightmapped(handle) => set_specular_uv(&mut lightmapped, handle, uv, |m| &m.extension.layers, |m| &mut m.extension.layers),
            SectionMaterial::MeshLit(handle) => set_specular_uv(&mut mesh_lit, handle, uv, |m| &m.extension.layers, |m| &mut m.extension.layers),
            SectionMaterial::Blended(handle) => set_specular_uv(&mut blended, handle, uv, |m| &m.extension.layers, |m| &mut m.extension.layers),
            SectionMaterial::Terrain(handle) => set_specular_uv(&mut terrain_layers, handle, uv, |m| &m.extension.layers, |m| &mut m.extension.layers),
            SectionMaterial::Standard(_) | SectionMaterial::TerrainPass(_) => {}
        }
    }
}
