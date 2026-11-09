use super::*;

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

pub fn restyle_switched(world: &mut bevy::ecs::world::World, looks: &HashMap<bevy::asset::AssetId<Image>, SwitchedLook>) {
    let mut restyled = restyle::<StandardMaterial>(world, looks, |m| m, |m| m, |_, _| {}, SectionMaterial::Standard);
    restyled.extend(restyle::<LightmappedMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), BakedLight::new(layers)),
        SectionMaterial::Lightmapped,
    ));
    restyled.extend(restyle::<MeshLitMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), DynamicLit::new(layers)),
        SectionMaterial::MeshLit,
    ));
    restyled.extend(restyle::<BlendedMaterial>(
        world,
        looks,
        |m| &m.base,
        |m| &mut m.base,
        |m, layers| (m.base, m.extension) = (layers.lit(m.base.clone()), OpaqueDecals::new(layers)),
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
    looks: &HashMap<bevy::asset::AssetId<Image>, SwitchedLook>,
    base: impl Fn(&M) -> &StandardMaterial,
    base_mut: impl Fn(&mut M) -> &mut StandardMaterial,
    layered: impl Fn(&mut M, &Layers),
    wrap: impl Fn(Handle<M>) -> SectionMaterial,
) -> Vec<(SectionMaterial, bevy::asset::AssetId<Image>)> {
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
        drawn.cull_mode = if drawn.double_sided { None } else { Some(Face::Back) };
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
