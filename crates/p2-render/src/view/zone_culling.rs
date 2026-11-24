use super::*;
use super::sky_copies::InSky;

pub(super) struct ZoneCullingPlugin;

impl Plugin for ZoneCullingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShadowOnly>().add_systems(Update, (follower_zones, layers_by_zone, zone_visibility, attachments_with_their_base).chain().in_set(SceneViewSystems::Zones));
    }
}

pub(super) fn follower_zones(
    mut commands: Commands,
    collision: Res<LevelCollision>,
    moved: Query<&ChildOf, (With<FollowActor>, Or<(Changed<Transform>, Changed<Aabb>)>)>,
    mut roots: Query<(&Children, Option<&mut ZoneCulled>), With<ActorModel>>,
    sections: Query<(&Transform, &Aabb, Has<crate::sprites::SpriteLook>), With<FollowActor>>,
) {
    let model = collision.model();
    let Some(model) = model.filter(|model| !model.leaves.is_empty()) else { return };
    let dirty: EntityHashSet = moved.iter().map(ChildOf::parent).collect();
    for root in dirty {
        let Ok((children, culled)) = roots.get_mut(root) else { continue };
        let corners = children.iter().filter_map(|&child| sections.get(child).ok()).flat_map(|(transform, aabb, sprite)| {
            let (center, half) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
            let any_facing = (half * transform.scale).length();
            (0..8).map(move |i| {
                let sign = Vec3::new([-1.0, 1.0][i & 1], [-1.0, 1.0][(i >> 1) & 1], [-1.0, 1.0][i >> 2]);
                to_bevy(if sprite { transform.translation + sign * any_facing } else { transform.transform_point(center + sign * half) })
            })
        });
        let Some((min, max)) = points_box(corners) else { continue };
        if culled.as_ref().is_some_and(|zone| zone.min == min && zone.max == max) {
            continue;
        }
        let zones = p2_collision::box_zones(model, min.into(), max.into());
        match culled {
            // The box is read every frame without change detection: only new zones need to wake `layers_by_zone`.
            Some(mut culled) if culled.zones == zones && culled.only_from.is_none() => {
                let held = culled.bypass_change_detection();
                (held.min, held.max) = (min, max);
            }
            Some(mut culled) => *culled = ZoneCulled { zones, min, max, only_from: None },
            None => {
                commands.entity(root).insert(ZoneCulled { zones, min, max, only_from: None });
            }
        }
    }
}

// No near plane: it would clip away a portal closer to the eye than that plane, and lose the zone behind.
pub(super) fn walk_view(transform: &Transform, projection: &Projection, origin: Vec3) -> Vec<Plane> {
    let frustum = projection.compute_frustum(&GlobalTransform::from(*transform));
    let sides = frustum.0.half_spaces.iter().enumerate().filter(|&(index, _)| index != bevy::shape::ViewFrustum::NEAR_PLANE_IDX);
    sides
        .map(|(_, half)| {
            // Bevy half-spaces (inside: n · p + d > 0) moved to Unreal space (p = swap(u) + origin).
            let normal = Vec3::from(half.normal());
            let [x, y, z] = to_bevy(normal);
            Plane::from([x, y, z, -(half.d() + normal.dot(origin))])
        })
        .collect()
}

/// Scenes without BSP portals, and a view outside the world, show everything. While the modern sun casts ([`SunShadow`]),
/// a moving sun caster ([`CASTER_LAYER`]) in the last cascade and every static one ([`STATIC_CASTER_LAYER`]) stay shown:
/// `Visibility::Hidden` takes a mesh out of the shadow pass too, so its shadow popped in and out with the view.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn zone_visibility(
    mut commands: Commands,
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    cameras: Query<(&Transform, &Projection), With<SceneView>>,
    suns: Query<&CascadeShadowConfig, With<SunShadow>>,
    layers: Query<&RenderLayers>,
    mut culled: Query<(Entity, &ZoneCulled, &mut Visibility, Option<&Children>, Option<&SeenWithin>, Has<CastsUnseen>)>,
    mut shadow_only: ResMut<ShadowOnly>,
    mut world_seen: ResMut<WorldSeenZones>,
    mut walk: bevy::ecs::system::Local<VisibleZones>,
    mut only_shadows: bevy::ecs::system::Local<bevy::utils::Parallel<Vec<Entity>>>,
) {
    shadow_only.0.clear();
    let model = collision.model();
    let view_zone = model.as_ref().zip(cameras.iter().next()).map(|(model, (transform, _))| {
        p2_collision::point_zone(model, to_bevy(transform.translation - shown.origin).into())
    });
    let mut eye_seen = None;
    let all = VisibleZones::default();
    let seen = match (&shown.portals, cameras.iter().next(), view_zone) {
        (Some(portals), Some((transform, projection)), Some(zone)) => {
            let eye = to_bevy(transform.translation - shown.origin);
            let view = walk_view(transform, projection, shown.origin);
            eye_seen = Some(eye);
            portals.visible(eye.into(), zone, &view, &mut walk);
            &*walk
        }
        _ => &all,
    };
    if seen.capped() {
        bevy::log::warn_once!("portal walk passed its cap: no zone culling at such views");
    }
    let mut seen_bits = [0u32; 8];
    for zone in 0..=u8::MAX {
        if seen.reached(zone) {
            seen_bits[usize::from(zone >> 5)] |= 1 << (zone & 31);
        }
    }
    world_seen.set_if_neq(WorldSeenZones(seen_bits));
    let sun = eye_seen.zip(suns.iter().next().and_then(|config| config.bounds.last().copied()));
    let caster = RenderLayers::layer(CASTER_LAYER);
    let cached_caster = RenderLayers::layer(STATIC_CASTER_LAYER);
    let unseen_flips = std::sync::Mutex::new(Vec::new());
    culled.par_iter_mut().for_each(|(root, zone, mut visibility, children, within, unseen_caster)| {
        let forced = forced_shown(zone.only_from.as_deref(), view_zone);
        let near = within.is_none_or(|within| eye_seen.is_none_or(|eye| in_reach(eye, zone, within.0)));
        let seen = forced && near && zone.zones.iter().any(|&z| Some(z) == shown.sky_zone || seen.sees(z, zone.min.into(), zone.max.into()));
        let own = layers.get(root).ok();
        let casts_on = |layer: &RenderLayers| {
            own.is_some_and(|own| own.intersects(layer)) || children.is_some_and(|children| children.iter().any(|&child| layers.get(child).is_ok_and(|l| l.intersects(layer))))
        };
        let drawn = seen || (forced && sun.is_some_and(|(eye, reach)| casts_on(&cached_caster) || (sun_reaches(eye, reach, zone) && casts_on(&caster))));
        let casts_unseen = drawn && !seen;
        if casts_unseen != unseen_caster
            && let Ok(mut flips) = unseen_flips.lock()
        {
            flips.push((root, casts_unseen));
        }
        if drawn && !seen {
            only_shadows.scope(|roots| roots.push(root));
        }
        visibility.set_if_neq(if drawn { Visibility::Inherited } else { Visibility::Hidden });
    });
    for roots in only_shadows.iter_mut() {
        shadow_only.0.extend(roots.drain(..));
    }
    for (root, casts_unseen) in unseen_flips.into_inner().unwrap_or_default() {
        commands.entity(root).queue_silenced(move |mut entity: bevy::ecs::world::EntityWorldMut| {
            if casts_unseen {
                entity.insert(CastsUnseen);
            } else {
                entity.remove::<CastsUnseen>();
            }
            let (from, to) = if casts_unseen { (0, UNSEEN_LAYER) } else { (UNSEEN_LAYER, 0) };
            entity.world_scope(|world| move_draws(world, root, from, to));
        });
    }
}

/// A caster in a zone the eye cannot see: its draws wait on [`UNSEEN_LAYER`] in place of the view's layer, and stay on
/// the sun's, so it keeps casting.
#[derive(Component)]
pub(super) struct CastsUnseen;

/// The engine culls an attachment by its own box. Ours has no zones, so it leaves the view with the model it is on,
/// and keeps casting with it.
pub(super) fn attachments_with_their_base(
    mut commands: Commands,
    models: Query<(Entity, &ActorModel)>,
    shown: Query<(&Visibility, Has<CastsUnseen>), With<ActorModel>>,
    attached: Query<(Entity, bevy::ecs::change_detection::Ref<FollowBone>, Option<&RenderLayers>, Option<&BaseModel>)>,
    spawned: Query<(), Added<ActorModel>>,
) {
    let retry = !spawned.is_empty();
    let mut by_actor: Option<(FxHashMap<ObjectId, Entity>, FxHashMap<ObjectId, ObjectId>)> = None;
    for (entity, follow, layers, base) in &attached {
        let known = base.filter(|_| !follow.is_changed()).map(|base| base.0).filter(|found| match found {
            Some(model) => shown.contains(*model),
            None => !retry,
        });
        let model = known.unwrap_or_else(|| {
            let (by_actor, bases) = by_actor.get_or_insert_with(|| {
                let mut bases = FxHashMap::default();
                for (_, other, ..) in &attached {
                    bases.entry(other.actor).or_insert(other.base);
                }
                (models.iter().map(|(entity, model)| (model.0, entity)).collect(), bases)
            });
            let mut on = follow.base;
            for _ in 0..4 {
                if by_actor.contains_key(&on) {
                    break;
                }
                let Some(&under) = bases.get(&on) else { break };
                on = under;
            }
            let found = by_actor.get(&on).copied();
            commands.entity(entity).insert(BaseModel(found));
            found
        });
        let unseen = model.and_then(|model| shown.get(model).ok()).is_some_and(|(visibility, casts_unseen)| *visibility == Visibility::Hidden || casts_unseen);
        let (from, to) = if unseen { (0, UNSEEN_LAYER) } else { (UNSEEN_LAYER, 0) };
        if layers.map_or(from == 0, |layers| layers.intersects(&RenderLayers::layer(from))) {
            commands.entity(entity).insert(layers.cloned().unwrap_or_default().without(from).with(to));
        }
    }
}

/// Moves every draw of `root` and under it from one layer to the other. A mesh without layers is on layer 0.
pub(super) fn move_draws(world: &mut bevy::ecs::world::World, root: Entity, from: usize, to: usize) {
    let mut draws = vec![root];
    let mut next = 0;
    while let Some(&draw) = draws.get(next) {
        next += 1;
        if let Some(children) = world.get::<Children>(draw) {
            draws.extend(children.iter().copied());
        }
        let unlayered = || world.get::<Mesh3d>(draw).map(|_| RenderLayers::default());
        let Some(layers) = world.get::<RenderLayers>(draw).cloned().or_else(unlayered) else { continue };
        if layers.intersects(&RenderLayers::layer(from)) {
            world.entity_mut(draw).insert(layers.without(from).with(to));
        }
    }
}

/// Sets the draws under a moving actor, a particle actor or a corona to the sky camera's layer, the view's or both,
/// when its zones change or a draw joins it (a particle pool grows).
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn layers_by_zone(
    mut commands: Commands,
    shown: Res<ShownScene>,
    changed: Query<Entity, (Or<(With<ActorModel>, With<LayeredByZone>)>, Changed<ZoneCulled>)>,
    joined: Query<Entity, (Or<(Added<ChildOf>, Added<Mesh3d>)>, Or<(With<Mesh3d>, With<RenderLayers>)>)>,
    parents: Query<&ChildOf>,
    roots: Query<(&ZoneCulled, Has<CastsUnseen>), Or<(With<ActorModel>, With<LayeredByZone>)>>,
    children: Query<&Children>,
    layers: Query<(Option<&RenderLayers>, Has<Mesh3d>, Has<InSky>)>,
) {
    let joined = joined.iter().flat_map(|entity| parents.iter_ancestors(entity)).filter(|&root| roots.contains(root));
    let dirty: EntityHashSet = changed.iter().chain(joined).collect();
    let scene = RenderLayers::layer(0).with(SKY_LAYER).with(UNSEEN_LAYER);
    for root in dirty {
        let Ok((zone, unseen)) = roots.get(root) else { continue };
        let world_layer = if unseen { UNSEEN_LAYER } else { 0 };
        let (in_sky, in_world) = sky_and_world(&zone.zones, shown.sky_zone);
        for draw in children.iter_descendants(root) {
            // As `move_draws`: an entity without layers is a draw only if it is a mesh, so a skeleton's joints are left be.
            let (now, was_in_sky) = match layers.get(draw) {
                Ok((Some(now), _, was_in_sky)) => (now.clone(), was_in_sky),
                Ok((None, true, was_in_sky)) => (RenderLayers::default(), was_in_sky),
                _ => continue,
            };
            // Draws on other layers only (an overlay's) are not the level's to move.
            if !now.intersects(&scene) && !was_in_sky {
                continue;
            }
            // The sky draws its own copy of a draw in the sky zone.
            let mut next = now.clone().without(0).without(UNSEEN_LAYER).without(SKY_LAYER);
            if in_world {
                next = next.with(world_layer);
            }
            if next != now {
                commands.entity(draw).insert(next);
            }
            if in_sky && !was_in_sky {
                commands.entity(draw).insert(InSky);
            } else if !in_sky && was_in_sky {
                commands.entity(draw).remove::<InSky>();
            }
        }
    }
}

pub(super) fn forced_shown(only_from: Option<&[u8]>, view_zone: Option<u8>) -> bool {
    only_from.is_none_or(|zones| view_zone.is_none_or(|view| zones.contains(&view)))
}

pub(super) fn sun_reaches(eye: [f32; 3], reach: f32, zone: &ZoneCulled) -> bool {
    let gap = |k: usize| eye[k].max(zone.min[k]).min(zone.max[k]) - eye[k];
    gap(0).powi(2) + gap(1).powi(2) + gap(2).powi(2) <= reach * reach
}

/// In Unreal units, from the eye to the middle of its `ZoneCulled` box: past this it is hidden, whatever its zone.
#[derive(Component)]
pub struct SeenWithin(pub f32);

fn in_reach(eye: [f32; 3], zone: &ZoneCulled, within: f32) -> bool {
    let mid = |axis: usize| 0.5 * (zone.min[axis] + zone.max[axis]);
    let d = [eye[0] - mid(0), eye[1] - mid(1), eye[2] - mid(2)];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= within * within
}

/// Left out of the sun's casters: grass and the like come in thousands of instances, and would be in every cascade.
#[derive(Component)]
pub struct NoSunCast;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reach_is_measured_to_the_middle_of_the_box() {
        let zone = ZoneCulled { zones: Vec::new(), min: [90.0, 0.0, 0.0], max: [110.0, 0.0, 0.0], only_from: None };
        assert!(in_reach([0.0; 3], &zone, 100.0));
        assert!(!in_reach([0.0; 3], &zone, 99.0));
    }

    #[test]
    fn a_box_moving_within_its_zones_leaves_them_unchanged() {
        use crate::level_collision::{LevelAlbedos, StaticCollision};
        let mut app = App::new();
        let model = two_zone_model();
        let collision = StaticCollision {
            hulls: p2_collision::BspHulls::new(&model),
            model,
            terrains: Vec::new(),
            meshes: Vec::new(),
            mesh_set: p2_collision::MeshSet::default(),
            lamp_casters: Vec::new(),
            lamp_masked: Vec::new(),
            albedos: LevelAlbedos::default(),
        };
        app.insert_resource(LevelCollision(Some(Arc::new(collision))));
        app.add_systems(Update, follower_zones);
        let follow = FollowActor { actor: ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
        let root = app.world_mut().spawn(ActorModel(follow.actor)).id();
        let aabb = Aabb::from_min_max(Vec3::splat(-10.0), Vec3::splat(10.0));
        let section = app.world_mut().spawn((follow, Transform::from_xyz(0.0, 50.0, 0.0), aabb, ChildOf(root))).id();
        let move_to = |app: &mut App, y: f32| {
            if let Some(mut transform) = app.world_mut().get_mut::<Transform>(section) {
                transform.translation.y = y;
            }
            app.update();
        };
        let changed = |app: &App| app.world().entity(root).get_change_ticks::<ZoneCulled>().map(|ticks| ticks.changed);
        let held = |app: &App| app.world().get::<ZoneCulled>(root).map(|zone| (zone.zones.clone(), zone.max));
        app.update();
        let (first, before) = (changed(&app), held(&app));
        move_to(&mut app, 51.0);
        let after = held(&app);
        assert_eq!(changed(&app), first);
        assert_eq!(after.as_ref().map(|(zones, _)| zones), before.as_ref().map(|(zones, _)| zones));
        assert_ne!(after.map(|(_, max)| max), before.map(|(_, max)| max), "the box follows");
        move_to(&mut app, -50.0);
        assert_ne!(changed(&app), first);
    }
}
