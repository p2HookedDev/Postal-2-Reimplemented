use super::*;

#[derive(Clone)]
pub struct MaterialImages {
    pub image: Handle<Image>,
    pub glow: Option<Handle<Image>>,
    pub detail: Option<Handle<Image>>,
    pub specular: Option<(Handle<Image>, crate::render::shader_layers::SpecularParams)>,
    pub state: RenderState,
}

#[derive(Clone)]
pub struct SectionView {
    pub mesh: Handle<Mesh>,
    pub texture: Option<Handle<Image>>,
    pub glow: Option<Handle<Image>>,
    pub layers: Layers,
    pub alpha_mode: AlphaMode,
    pub no_scene_lights: bool,
    pub fullbright: bool,
    pub depth_bias: f32,
    pub lightmap: Option<Handle<Image>>,
    pub two_sided: bool,
    pub uv: Vec<UvModifier>,
    pub skin: Option<Skin>,
    pub tint: [f32; 4],
    pub alpha_map: Option<Handle<Image>>,
    pub terrain: Vec<TerrainPassLayer>,
    pub chunks: Option<Arc<[crate::render::world_pass::Chunk]>>,
}

#[derive(Clone)]
pub struct Skin {
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    pub bind: Arc<[Transform]>,
}

impl SectionView {
    pub fn textured(mesh: Handle<Mesh>, found: Option<MaterialImages>) -> Self {
        let state = found.as_ref().map(|found| found.state.clone()).unwrap_or_default();
        let blending = match state.blending.unwrap_or(Blending::Opaque) {
            Blending::Opaque | Blending::Masked if state.color_blend => Blending::Alpha,
            blending => blending,
        };
        let alpha_mode = match blending {
            Blending::Opaque => AlphaMode::Opaque,
            // Texels pass above the reference: half a step over it keeps an alpha equal to it out.
            Blending::Masked => AlphaMode::Mask((f32::from(state.alpha_ref.unwrap_or(127)) + 0.5) / 255.0),
            Blending::Alpha => AlphaMode::Blend,
            Blending::Additive => AlphaMode::Add,
            Blending::Modulate => AlphaMode::Multiply,
            Blending::Invisible => AlphaMode::Mask(2.0),
        };
        Self {
            mesh,
            glow: found.as_ref().and_then(|found| found.glow.clone()),
            layers: Layers::new(
                found.as_ref().and_then(|found| found.detail.clone()),
                state.detail.map_or(0.0, |detail| detail.scale),
                found.as_ref().and_then(|found| found.specular.clone()),
                state.specular_uv.clone(),
                state.opacity.is_some() && (state.glow.is_some() || state.specular.is_some()),
            ),
            texture: found.map(|found| found.image),
            alpha_mode,
            no_scene_lights: state.unlit,
            fullbright: state.unlit,
            depth_bias: 0.0,
            lightmap: None,
            two_sided: state.two_sided,
            uv: state.uv,
            skin: None,
            tint: state.color.map_or([1.0; 4], |color| color.map(|c| (f32::from(c) / 255.0).powf(2.2))),
            alpha_map: None,
            terrain: Vec::new(),
            chunks: None,
        }
    }

    pub fn emissive(&self) -> LinearRgba {
        if self.glow.is_some() { LinearRgba::WHITE } else { LinearRgba::BLACK }
    }

    pub fn material_key(&self) -> String {
        format!(
            "{:?} {:?} {} {:?} {} {} {:?} {} {:?} {:?} {:?} {:?}",
            self.texture.as_ref().map(Handle::id),
            self.glow.as_ref().map(Handle::id),
            self.layers.key(),
            self.alpha_mode,
            self.no_scene_lights,
            self.depth_bias,
            self.lightmap.is_some(),
            self.two_sided,
            self.uv,
            self.tint,
            self.alpha_map.as_ref().map(Handle::id),
            self.terrain
        )
    }
}

#[derive(Component, Clone, Copy)]
pub struct SkinJoint {
    pub actor: ObjectId,
    pub bone: usize,
}

#[expect(clippy::too_many_arguments, reason = "one call per kind of actor model; the parts differ per caller")]
pub fn spawn_actor_sections(
    commands: &mut Commands,
    parent: Entity,
    sections: Vec<SectionView>,
    transform: Transform,
    (actor, follow): (ObjectId, impl Component + Clone),
    mut material: impl FnMut(&SectionView) -> SectionMaterial,
    lit: Option<ActorLit>,
    extra: impl Bundle + Clone,
) {
    let mut skin: Option<SkinnedMesh> = None;
    for section in sections {
        let mut spawned = commands.spawn((Mesh3d(section.mesh.clone()), transform, follow.clone(), extra.clone(), ChildOf(parent)));
        material(&section).insert(&mut spawned);
        let entity = spawned.id();
        if let Some(lit) = &lit {
            commands.entity(entity).insert(lit.clone().with_layers(section.layers.clone()));
        }
        let Some(Skin { inverse_bindposes, bind }) = section.skin.clone() else { continue };
        let skinned = skin.get_or_insert_with(|| {
            let joints = (bind.iter().enumerate())
                .map(|(bone, &at)| commands.spawn((at, SkinJoint { actor, bone }, ChildOf(entity))).id())
                .collect();
            SkinnedMesh { inverse_bindposes, joints }
        });
        // Bounds follow the posed joints (frustum and occlusion culling), not the bind pose: recomputed when the
        // actor's pose changes (`animation::skinned_bounds`), not every frame as Bevy's `DynamicSkinnedMeshBounds`.
        commands.entity(entity).insert(skinned.clone());
    }
}
