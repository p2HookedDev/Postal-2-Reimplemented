use super::*;

#[derive(Default)]
pub(crate) struct PawnParts {
    pub(crate) followers: Vec<MovingModel<FollowActor>>,
    pub(crate) attached: Vec<MovingModel<FollowBone>>,
    pub(crate) animated: Vec<AnimatedMesh>,
    pub(crate) drawn_pawns: usize,
    pub(crate) drawn_attachments: usize,
}

pub(super) fn pawn_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    animations: &mut HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    vm_animations: &VmAnimations,
    pawns: HashMap<(String, Vec<Option<String>>), Vec<SkeletalActor>>,
    attachments: &HashMap<ObjectId, Vec<SkeletalActor>>,
) -> PawnParts {
    let mut parts = PawnParts::default();
    let mut attached_instances: HashMap<(String, Vec<Option<String>>), Vec<(FollowBone, Mat4, ActorLighting)>> = HashMap::new();
    for ((mesh_path, skins), actors) in pawns {
        let Some((mesh, mesh_id, sections, handles)) = skeletal_sections(game, t, texture, &mesh_path, &skins)
        else {
            continue;
        };
        let mesh = Arc::new(mesh);
        let animated = {
            let (pkg, data) = game.data(mesh_id);
                let animations = animations.entry(mesh_id).or_insert_with(|| vm_animations.package(game.package_name(mesh_id), pkg, data));
                animate_mesh_with(pkg, animations, handles.clone(), &mesh, &["idle", "stand"])
        };
        let pose = match &animated {
            Some((item, _)) => item.bone_pose(0.0),
            None => {
                let parents: Vec<usize> = mesh.bones.iter().map(|b| b.parent).collect();
                let local: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
                bone_matrices(&parents, &local)
            }
        };
        let attached = actors.iter().any(|a| attachments.contains_key(&a.object));
        let bone_names: Vec<String> = if attached {
            let (pkg, _) = game.data(mesh_id);
            mesh.bones.iter().map(|b| pkg.name(b.name).to_owned()).collect()
        } else {
            Vec::new()
        };
        for a in &actors {
            let own_sections = sections.clone();
            if let Some((item, _)) = &animated {
                let mut item = item.clone();
                item.actor = Some(a.object);
                parts.animated.push(item);
            }
            let body = unreal_mesh_matrix(
                a.location,
                rotator_matrix(a.rotation),
                a.scale,
                a.pre_pivot,
                mesh.scale,
                mesh.origin,
                mesh.rot_origin,
            );
            let follow = FollowActor {
                actor: a.object,
                scale: a.scale,
                pre_pivot: a.pre_pivot,
                skeletal: Some((mesh.scale, mesh.origin, mesh.rot_origin)),
            };
            // Lit by the lights where it walks (one colour per pawn: the meshes are shared).
            let lit = ActorLit::skeletal(a.object, &mesh, a.scale, &a.lighting);
            parts.followers.push(MovingModel { sections: own_sections, transform: unreal_to_bevy(body), follow, lit });
            for attachment in attachments.get(&a.object).into_iter().flatten() {
                let Some((_, bone)) = &attachment.attached_to else { continue };
                let Some(index) = bone_names.iter().position(|n| n.eq_ignore_ascii_case(bone)) else { continue };
                let Some(&bone) = pose.get(index) else { continue };
                let follow = FollowBone {
                    actor: attachment.object,
                    base: a.object,
                    bone: index,
                    body,
                    base_follow: follow,
                    relative_location: attachment.relative_location,
                    relative_rotation: attachment.relative_rotation,
                    scale: attachment.scale,
                    pre_pivot: attachment.pre_pivot,
                    mesh_scale: [1.0; 3].into(),
                    origin: [0.0; 3].into(),
                    rot_origin: [0; 3].into(),
                };
                attached_instances.entry((attachment.mesh.clone(), attachment.skins.clone())).or_default().push((follow, bone, attachment.lighting));
            }
        }
        parts.drawn_pawns += actors.len();
    }
    for ((mesh_path, skins), followers) in attached_instances {
        let Some((mesh, mesh_id, sections, handles)) = skeletal_sections(game, t, texture, &mesh_path, &skins) else { continue };
        let mesh = Arc::new(mesh);
        let animated = {
            let (pkg, data) = game.data(mesh_id);
            let animations = animations.entry(mesh_id).or_insert_with(|| vm_animations.package(game.package_name(mesh_id), pkg, data));
            animate_mesh_with(pkg, animations, handles.clone(), &mesh, &["idle"]).map(|(item, _)| item).unwrap_or_else(|| still_mesh(pkg, handles.clone(), &mesh))
        };
        for (follow, bone, lighting) in followers {
            let mut item = animated.clone();
            item.actor = Some(follow.actor);
            parts.animated.push(item);
            let follow = FollowBone { mesh_scale: mesh.scale, origin: mesh.origin, rot_origin: mesh.rot_origin, ..follow };
            let lit = ActorLit::skeletal(follow.base, &mesh, follow.scale, &lighting);
            parts.attached.push(MovingModel { sections: sections.clone(), transform: follow.transform(follow.body, bone), follow, lit });
            parts.drawn_attachments += 1;
        }
    }
    parts
}
