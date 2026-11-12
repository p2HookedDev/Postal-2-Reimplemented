use super::*;

pub(super) struct HudTargets<'a, 'w, 's> {
    pub(super) assets: &'a mut AssetStores<'w>,
    pub(super) spawned: &'a mut SpawnedActors,
    pub(super) hud: &'a mut Hud,
    pub(super) animated: &'a mut Animated,
    pub(super) play: &'a mut Play,
    pub(super) view: &'a mut MeshView,
    pub(super) shown: &'a mut ResMut<'w, crate::shown_scene::ShownScene>,
    pub(super) canvas: &'a mut CanvasModels<'w, 's>,
    pub(super) commands: &'a mut Commands<'w, 's>,
}

#[derive(SystemParam)]
pub(super) struct HudParams<'w, 's> {
    session: ResMut<'w, Session>,
    assets: AssetStores<'w>,
    spawned: ResMut<'w, SpawnedActors>,
    hud: ResMut<'w, Hud>,
    animated: ResMut<'w, Animated>,
    play: ResMut<'w, Play>,
    view: ResMut<'w, MeshView>,
    shown: ResMut<'w, crate::shown_scene::ShownScene>,
    canvas: CanvasModels<'w, 's>,
    commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

impl<'w, 's> SceneTargets<'w, 's> {
    pub(super) fn hud_targets(&mut self) -> HudTargets<'_, 'w, 's> {
        HudTargets {
            assets: &mut self.assets,
            spawned: &mut self.spawned,
            hud: &mut self.hud,
            animated: &mut self.animated,
            play: &mut self.play,
            view: &mut self.view,
            shown: &mut self.shown,
            canvas: &mut self.canvas,
            commands: &mut self.commands,
        }
    }
}

pub(super) fn drop_left_canvas_actors_when_presented(play: Res<Play>, left: Query<Entity, With<LeftCanvasActor>>, mut commands: Commands) {
    if play.world().is_some_and(PlayWorld::presented) {
        for entity in &left {
            commands.entity(entity).despawn();
        }
    }
}

pub(super) fn present_canvas_actors(mut t: HudParams) {
    let items = t.play.world().filter(|world| world.presented()).map(PlayWorld::hud_items).unwrap_or_default();
    let started = std::time::Instant::now();
    t.session.show_hud_actors(&items, &mut HudTargets {
        assets: &mut t.assets,
        spawned: &mut t.spawned,
        hud: &mut t.hud,
        animated: &mut t.animated,
        play: &mut t.play,
        view: &mut t.view,
        shown: &mut t.shown,
        canvas: &mut t.canvas,
        commands: &mut t.commands,
    });
    t.times.add("canvas actors", started);
}

impl Session {
    pub(super) fn show_hud_actors(&mut self, items: &[CanvasItem], t: &mut HudTargets) {
        let calls = items.iter().filter(|item| matches!(item, CanvasItem::Actor { .. }));
        let drawn: Vec<(&CanvasItem, usize)> = calls.zip(hud::call_slots(items)).filter(|&(_, slot)| slot < OVERLAY_LAYERS.len()).collect();
        t.view.fov = t.play.world().and_then(PlayWorld::fov).unwrap_or(0.0);
        if let Some(fogs) = t.play.world_mut().and_then(PlayWorld::take_zone_fogs) {
            t.shown.fogs = fogs;
        }
        let volume_fog = t.play.world().and_then(PlayWorld::volume_fog);
        if t.shown.volume_fog != volume_fog {
            t.shown.volume_fog = volume_fog;
        }
        if t.canvas.left.is_empty() || t.play.world().is_some_and(PlayWorld::presented) {
            t.view.overlays.clear();
            for &(item, slot) in &drawn {
                if let CanvasItem::Actor { fov, .. } = *item
                    && slot == t.view.overlays.len()
                {
                    t.view.overlays.push(fov);
                }
            }
        }
        self.show_canvas_actors(&drawn, t);
    }

    /// The models are placed where the HUD's scripts put their actors at the call (`RenderOverlays` moves the weapon to the
    /// view, `MapScreen` moves its trail marker from point to point), not where they were before the HUD.
    pub(super) fn show_canvas_actors(&mut self, drawn: &[(&CanvasItem, usize)], t: &mut HudTargets) {
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
                        let layer = RenderLayers::layer(OVERLAY_LAYERS[now]);
                        for (section, _) in t.canvas.canvas_sections.iter().filter(|(_, parent)| parent.parent() == *entity) {
                            t.commands.entity(section).insert(layer.clone());
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
        // Retail draws a skeletal actor's attachments in its own scene node, so they go on the overlay layer
        // of its first call. With the base no longer HUD-drawn (dropped, detached), they return to the world.
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
        let overlays = RenderLayers::from_layers(&OVERLAY_LAYERS);
        for (entity, follow, mut transform, mut shown, layers) in &mut t.canvas.attached {
            let on_overlay = layers.is_some_and(|layers| layers.intersects(&overlays));
            if !on_overlay && !world.attached(follow.actor) {
                continue;
            }
            let Some(actor) = world.actor(follow.actor) else { continue };
            let base = actor.attachment.as_ref().and_then(|attachment| t.hud.canvas_actors.iter().find(|drawn| drawn.actor == attachment.base && drawn.call == 0));
            let Some(base) = base else {
                if on_overlay {
                    t.commands.entity(entity).try_insert(RenderLayers::default());
                }
                continue;
            };
            let visibility = if base.slot.is_none() || actor.hidden { Visibility::Hidden } else { Visibility::Inherited };
            if *shown != visibility {
                *shown = visibility;
            }
            let Some(slot) = base.slot else { continue };
            let layer = RenderLayers::layer(OVERLAY_LAYERS[slot]);
            if layers != Some(&layer) {
                t.commands.entity(entity).try_insert(layer);
            }
            if let (Some(&moved), Some(frame)) = (moves.get(&base.actor), world.shown.places.bone_frame(follow.actor)) {
                let placed = crate::animation::attached_transform(follow, moved * frame.matrix());
                if *transform != placed {
                    *transform = placed;
                }
            }
        }
    }

    pub(super) fn spawn_canvas_actor(&mut self, actor: ObjectId, call: usize, slot: usize, item: &CanvasItem, t: &mut HudTargets) -> Option<Entity> {
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
        let Some((mesh, mesh_id, mut sections, handles)) = skeletal_sections(game, t.assets, &mut material_images::<AssetStores>, &path, &skins) else {
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
        let layer = RenderLayers::layer(OVERLAY_LAYERS[slot]);
        let parent = t.commands.spawn((MapScoped, Transform::from_translation(origin), Visibility::default())).id();
        let materials = &mut t.assets.materials;
        let material = |section: &SectionView| {
            SectionMaterial::Standard(materials.add(crate::mesh_view::actor_standard(section, crate::mesh_view::SceneLights::Ignored)))
        };
        let lighting = ActorLighting { unlit: false, glow: 0, max_lights: DEFAULT_MAX_LIGHTS };
        let lit = ActorLit::skeletal(actor, &mesh, follow.scale, &lighting);
        spawn_actor_sections(t.commands, parent, sections, placed(follow), (follow.actor, follow), material, lit, (layer, CanvasDrawn(call)));
        Some(parent)
    }
}
