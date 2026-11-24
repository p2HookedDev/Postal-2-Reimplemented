use super::*;
use std::collections::HashSet;
use bevy::asset::AssetId;

pub struct SwitchedLook {
    pub was: SectionView,
    pub now: SectionView,
    pub size: [f32; 2],
    pub specular_size: [f32; 2],
}

pub(super) fn surface_alpha_mode(drawn: AlphaMode, was: AlphaMode, now: AlphaMode) -> AlphaMode {
    let plain = |mode: AlphaMode| if mode == AlphaMode::Premultiplied { AlphaMode::Blend } else { mode };
    if plain(drawn) == plain(was) { now } else { drawn }
}

#[expect(clippy::fn_params_excessive_bools, reason = "three states of one surface side flag")]
pub(super) fn surface_two_sided(drawn: bool, was: bool, now: bool) -> bool {
    now || (drawn && !was)
}

pub fn restyle_switched(world: &mut bevy::ecs::world::World, looks: &HashMap<AssetId<Image>, SwitchedLook>) {
    let mut restyled = restyle::<StandardMaterial>(world, looks, |m| m, |m| m, |_, _| {}, SectionMaterial::Standard);
    restyled.extend(restyle::<SkyStandardMaterial>(world, looks, |m| &m.base, |m| &mut m.base, |_, _| {}, SectionMaterial::SkyStandard));
    restyled.extend(restyle::<LightmappedMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), BakedLight { sky: m.extension.sky, ..BakedLight::new(layers) }),
        SectionMaterial::Lightmapped,
    ));
    restyled.extend(restyle::<MeshLitMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), DynamicLit { sky: m.extension.sky, ..DynamicLit::new(layers) }),
        SectionMaterial::MeshLit,
    ));
    restyled.extend(restyle::<BlendedMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), OpaqueDecals { sky: m.extension.sky, ..OpaqueDecals::new(layers) }),
        SectionMaterial::Blended,
    ));
    let Some(mut animated) = world.get_resource_mut::<AnimatedMaterials>() else { return };
    for (material, image) in restyled {
        let Some(SwitchedLook { now, size, specular_size, .. }) = looks.get(&image) else { continue };
        animated.textures.retain(|moving| moving.material != material);
        animated.speculars.retain(|moving| moving.material != material);
        if !now.uv.is_empty() {
            animated.textures.push(MovingUv { material: material.clone(), modifiers: now.uv.clone(), size: *size });
        }
        if now.layers.flat_specular.is_some() && !now.layers.specular_uv.is_empty() {
            animated.speculars.push(MovingUv { material, modifiers: now.layers.specular_uv.clone(), size: *specular_size });
        }
    }
}

fn restyle<M: bevy::asset::Asset>(
    world: &mut bevy::ecs::world::World,
    looks: &HashMap<AssetId<Image>, SwitchedLook>,
    base: impl Fn(&M) -> &StandardMaterial,
    base_mut: impl Fn(&mut M) -> &mut StandardMaterial,
    layered: impl Fn(&mut M, &Layers),
    wrap: impl Fn(Handle<M>) -> SectionMaterial,
) -> Vec<(SectionMaterial, AssetId<Image>)> {
    let Some(mut store) = world.get_resource_mut::<Assets<M>>() else { return Vec::new() };
    let drawing: Vec<_> = store
        .iter()
        .filter_map(|(id, material)| {
            let image = base(material).base_color_texture.as_ref()?.id();
            looks.contains_key(&image).then_some((id, image))
        })
        .collect();
    let mut restyled = Vec::new();
    for (id, image) in drawing {
        let (Some(look), Some(handle)) = (looks.get(&image), store.get_strong_handle(id)) else { continue };
        let Some(mut material) = store.get_mut(id) else { continue };
        let material = &mut *material;
        let drawn = base_mut(material);
        drawn.alpha_mode = surface_alpha_mode(drawn.alpha_mode, look.was.alpha_mode, look.now.alpha_mode);
        drawn.double_sided = surface_two_sided(drawn.double_sided, look.was.two_sided, look.now.two_sided);
        drawn.cull_mode = cull_for(drawn.double_sided);
        let [r, g, b, a] = look.now.tint;
        drawn.base_color = Color::linear_rgba(r, g, b, a);
        drawn.emissive = look.now.emissive();
        drawn.emissive_texture.clone_from(&look.now.glow);
        drawn.uv_transform = Affine2::IDENTITY;
        layered(material, &look.now.layers);
        restyled.push((wrap(handle), image));
    }
    restyled
}

/// A prepared material holds the view of the texture it was prepared with,
/// so it shows a switched image's new texture only once prepared again.
pub fn rebind_materials(world: &mut bevy::ecs::world::World, images: &HashSet<AssetId<Image>>) {
    touch_drawing::<StandardMaterial>(world, images, |material| material);
    touch_drawing::<SkyStandardMaterial>(world, images, |material| &material.base);
    touch_drawing::<MeshLitMaterial>(world, images, |material| &material.base);
    touch_drawing::<LightmappedMaterial>(world, images, |material| &material.base);
    touch_drawing::<crate::actor_light::ActorLitMaterial>(world, images, |material| &material.base);
    touch_drawing::<BlendedMaterial>(world, images, |material| &material.base);
    touch_drawing::<TerrainLayerMaterial>(world, images, |material| &material.base);
}

fn touch_drawing<M: bevy::asset::Asset>(
    world: &mut bevy::ecs::world::World,
    images: &HashSet<AssetId<Image>>,
    base: impl Fn(&M) -> &StandardMaterial,
) {
    let Some(mut store) = world.get_resource_mut::<Assets<M>>() else { return };
    let drawing: Vec<AssetId<M>> = store
        .iter()
        .filter(|(_, material)| base(material).base_color_texture.as_ref().is_some_and(|image| images.contains(&image.id())))
        .map(|(id, _)| id)
        .collect();
    for id in drawing {
        if let Some(material) = store.get_mut(id) {
            material.into_inner();
        }
    }
}
