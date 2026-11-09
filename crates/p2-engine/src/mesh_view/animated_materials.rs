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
        let (u, v) = (Vec4::new(a, b, c, 0.0), Vec4::new(d, e, f, 0.0));
        match material {
            SectionMaterial::Lightmapped(handle) => lightmapped.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::MeshLit(handle) => mesh_lit.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::Blended(handle) => blended.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::Terrain(handle) => terrain_layers.get_mut(handle).map(|mut m| (m.extension.layers.specular_u, m.extension.layers.specular_v) = (u, v)),
            SectionMaterial::Standard(_) | SectionMaterial::TerrainPass(_) => None,
        };
    }
}
