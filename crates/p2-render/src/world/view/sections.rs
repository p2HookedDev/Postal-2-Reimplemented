use super::*;

#[expect(clippy::too_many_arguments, reason = "one call per kind of actor model; the parts differ per caller")]
pub fn spawn_actor_sections(
    commands: &mut Commands,
    parent: Entity,
    sections: Vec<SectionView>,
    transform: Transform,
    (actor, follow): (ObjectId, impl Component + Clone),
    (materials, look): (&mut ActorMaterials, ActorLook),
    lit: Option<ActorLit>,
    extra: impl Bundle + Clone,
) {
    let mut skin: Option<SkinnedMesh> = None;
    for section in sections {
        let mut spawned = commands.spawn((Mesh3d(section.mesh.clone()), transform, follow.clone(), extra.clone(), ChildOf(parent)));
        materials.material(look, &section).insert(&mut spawned);
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
        commands.entity(entity).insert(skinned.clone());
    }
}
