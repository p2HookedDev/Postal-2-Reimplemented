use super::*;
use p2_render::frame_facts::HudView;

#[derive(SystemParam)]
pub(super) struct HudParams<'w, 's> {
    pub(super) assets: AssetStores<'w>,
    pub(super) spawned: ResMut<'w, SpawnedActors>,
    pub(super) hud: ResMut<'w, Hud>,
    animated: ResMut<'w, Animated>,
    play: ResMut<'w, Play>,
    hud_view: ResMut<'w, HudView>,
    canvas: CanvasModels<'w, 's>,
    pub(super) commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn drop_left_canvas_actors_when_presented(play: Res<Play>, left: Query<Entity, With<LeftCanvasActor>>, mut commands: Commands) {
    if play.world().is_some_and(PlayWorld::presented) {
        for entity in &left {
            commands.entity(entity).despawn();
        }
    }
}

pub(super) fn present_canvas_actors(mut session: ResMut<Session>, mut t: HudParams) {
    let items = t.play.world().filter(|world| world.presented()).map(PlayWorld::hud_items).unwrap_or_default();
    let started = std::time::Instant::now();
    session.show_hud_actors(&items, &mut t);
    t.times.add("canvas actors", started);
}

impl Session {
    pub(super) fn show_hud_actors(&mut self, items: &[CanvasItem], t: &mut HudParams) {
        let calls = items.iter().filter(|item| matches!(item, CanvasItem::Actor { .. }));
        let drawn: Vec<(&CanvasItem, usize)> = calls.zip(p2_vm::call_slots(items)).filter(|&(_, slot)| slot < OVERLAY_LAYERS.len()).collect();
        if t.canvas.left.is_empty() || t.play.world().is_some_and(PlayWorld::presented) {
            t.hud_view.overlays.clear();
            for &(item, slot) in &drawn {
                if let CanvasItem::Actor { fov, .. } = *item
                    && slot == t.hud_view.overlays.len()
                {
                    t.hud_view.overlays.push(fov);
                }
            }
        }
        self.show_canvas_actors(&drawn, t);
    }

    pub(super) fn show_canvas_actors(&mut self, drawn: &[(&CanvasItem, usize)], t: &mut HudParams) {
        let Some(world) = t.play.world() else { return };
        t.hud.canvas_actors.retain(|shown| {
            let keep = !world.is_gone(shown.actor);
            if !keep {
                t.animated.0.retain(|item| item.actor != Some(shown.actor));
                t.commands.entity(shown.entity).despawn();
            }
            keep
        });
        let calls: Vec<(ObjectId, usize, usize, &CanvasItem)> = drawn
            .iter()
            .enumerate()
            .filter_map(|(at, &(item, slot))| match *item {
                CanvasItem::Actor { actor, .. } => {
                    let before = drawn[..at].iter().filter(|(other, _)| matches!(other, CanvasItem::Actor { actor: a, .. } if *a == actor)).count();
                    Some((actor, before, slot, item))
                }
                _ => None,
            })
            .collect();
        let call_of = |actor: ObjectId, call: usize| calls.iter().find(|&&(a, n, ..)| a == actor && n == call);
        for &(actor, call, slot, item) in &calls {
            if !t.hud.canvas_actors.iter().any(|shown| shown.actor == actor && shown.call == call)
                && let Some(entity) = self.spawn_canvas_actor(actor, call, slot, item, t)
            {
                t.hud.canvas_actors.push(CanvasActor { actor, call, entity, slot: Some(slot) });
            }
        }
        for CanvasActor { actor, call, entity, slot } in &mut t.hud.canvas_actors {
            let now = call_of(*actor, *call).map(|&(_, _, slot, _)| slot);
            if *slot == now {
                continue;
            }
            match now {
                Some(now) => {
                    t.commands.entity(*entity).insert(Visibility::Inherited);
                    if slot.is_none_or(|before| before != now) {
                        for (section, _) in t.canvas.canvas_sections.iter().filter(|(_, parent)| parent.parent() == *entity) {
                            t.commands.entity(section).insert(OverlaySlot(Some(now)));
                        }
                    }
                }
                None => {
                    t.commands.entity(*entity).insert(Visibility::Hidden);
                }
            }
            *slot = now;
        }
        for (follow, drawn, mut transform) in &mut t.canvas.canvas_models {
            let Some(&(_, _, _, &CanvasItem::Actor { location, rotation, scale, .. })) = call_of(follow.actor, drawn.0) else { continue };
            let placed = FollowActor { scale, ..*follow }.transform(location, rotation);
            if *transform != placed {
                *transform = placed;
            }
        }
        let Some(world) = t.play.world() else { return };
        let moves: FxHashMap<ObjectId, Mat4> = t.canvas.canvas_models.iter()
            .filter(|(_, drawn, _)| drawn.0 == 0)
            .filter_map(|(follow, _, _)| {
                let Some(&(_, _, _, &CanvasItem::Actor { location, rotation, scale, .. })) = call_of(follow.actor, 0) else { return None };
                let (stands, turned) = world.tick_place(follow.actor)?;
                let left = follow.mesh_matrix(stands, turned)?;
                Some((follow.actor, FollowActor { scale, ..*follow }.mesh_matrix(location, rotation)? * left.inverse()))
            })
            .collect();
        for (entity, follow, mut transform, mut shown, at) in &mut t.canvas.attached {
            let on_overlay = at.is_some_and(|at| at.0.is_some());
            if !on_overlay && !world.attached(follow.actor) {
                continue;
            }
            let Some(actor) = world.actor(follow.actor) else { continue };
            let base = actor.attachment.as_ref().and_then(|attachment| t.hud.canvas_actors.iter().find(|drawn| drawn.actor == attachment.base && drawn.call == 0));
            let Some(base) = base else {
                if on_overlay {
                    t.commands.entity(entity).try_insert(OverlaySlot(None));
                }
                continue;
            };
            let visibility = if base.slot.is_none() || actor.hidden { Visibility::Hidden } else { Visibility::Inherited };
            if *shown != visibility {
                *shown = visibility;
            }
            let Some(slot) = base.slot else { continue };
            let wanted = OverlaySlot(Some(slot));
            if at != Some(&wanted) {
                t.commands.entity(entity).try_insert(wanted);
            }
            if let (Some(&moved), Some(frame)) = (moves.get(&base.actor), world.shown.places.bone_frame(follow.actor)) {
                let placed = crate::animation::attached_transform(follow, moved * frame.matrix());
                if *transform != placed {
                    *transform = placed;
                }
            }
        }
    }

    pub(super) fn spawn_canvas_actor(&mut self, actor: ObjectId, call: usize, slot: usize, item: &CanvasItem, t: &mut HudParams) -> Option<Entity> {
        let placed = |follow: FollowActor| match *item {
            CanvasItem::Actor { location, rotation, scale, .. } => FollowActor { scale, ..follow }.transform(location, rotation),
            _ => Transform::default(),
        };
        if let Some((a, origin)) = t.play.world().and_then(|world| world.canvas_static(actor).map(|a| (a.clone(), world.origin()))) {
            return self.spawn_canvas_static(&a, origin, call, slot, placed, t);
        }
        let Some((path, skins, follow, origin)) =
            t.play.world().and_then(|world| world.canvas_mesh(actor).map(|mesh| (mesh.path.clone(), mesh.skins.clone(), mesh.follow, world.origin())))
        else {
            bevy::log::warn_once!("HUD actor {actor:?}: no mesh read for it");
            return None;
        };
        if t.hud.failed_canvas_meshes.contains(&path) {
            return None;
        }
        let game = open_game(&mut self.library, self.map_system.as_deref())?;
        let Some((mesh, mesh_id, mut sections, handles)) = skeletal_sections(game, &mut t.assets, &mut material_images::<AssetStores>, &path, &skins) else {
            bevy::log::warn!("HUD actor {actor:?}: skeletal mesh {path} not built");
            t.hud.failed_canvas_meshes.insert(path);
            return None;
        };
        let mesh = Arc::new(mesh);
        bevy::log::info!("HUD actor {actor:?}: {path}, {} sections, overlay slot {slot}", sections.len());
        let follow = FollowActor { skeletal: Some((mesh.scale, mesh.origin, mesh.rot_origin)), ..follow };
        let posed = {
            let (pkg, data) = game.data(mesh_id);
            let animations = self.animations.entry(mesh_id).or_insert_with(|| self.vm_animations.package(game.package_name(mesh_id), pkg, data));
            animate_mesh_with(pkg, animations, handles, &mesh, &["idle"])
        };
        if let Some((mut item, _)) = posed {
            item.actor = Some(actor);
            if let Some(joints) = t.play.world().and_then(|world| crate::animation::first_joints(&mut item, world)) {
                for skin in sections.iter_mut().filter_map(|section| section.skin.as_mut()).filter(|skin| skin.bind.len() == joints.len()) {
                    skin.bind = Arc::clone(&joints);
                }
            }
            t.animated.0.push(item);
        }
        let parent = t.commands.spawn((MapScoped, Transform::from_translation(origin), Visibility::default())).id();
        let lighting = ActorLighting { unlit: false, glow: 0, max_lights: DEFAULT_MAX_LIGHTS };
        let lit = ActorLit::skeletal(actor, &mesh, follow.scale, &lighting);
        spawn_actor_sections(&mut t.commands, parent, sections, placed(follow), (follow.actor, follow), (&mut t.assets.actor_materials, ActorLook::Canvas), lit, (OverlaySlot(Some(slot)), CanvasDrawn(call)));
        Some(parent)
    }
}
