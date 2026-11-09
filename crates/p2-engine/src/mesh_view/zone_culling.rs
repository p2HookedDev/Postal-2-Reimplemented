use super::*;

pub(crate) fn merged_box((min, max): ([f32; 3], [f32; 3]), (low, high): ([f32; 3], [f32; 3])) -> ([f32; 3], [f32; 3]) {
    ([0, 1, 2].map(|k| min[k].min(low[k])), [0, 1, 2].map(|k| max[k].max(high[k])))
}

pub(crate) fn bounds(points: impl IntoIterator<Item = [f32; 3]>) -> Option<([f32; 3], [f32; 3])> {
    points.into_iter().fold(None, |bounds, p| Some(merged_box(bounds.unwrap_or((p, p)), (p, p))))
}

pub(super) fn follower_zones(
    mut commands: Commands,
    collision: Res<LevelCollision>,
    moved: Query<&ChildOf, (With<FollowActor>, Or<(Changed<Transform>, Changed<Aabb>)>)>,
    mut roots: Query<(&Children, Option<&mut ZoneCulled>), With<ActorModel>>,
    sections: Query<(&Transform, &Aabb, Has<crate::map::SpriteQuad>), With<FollowActor>>,
) {
    let model = collision.model();
    let Some(model) = model.filter(|model| !model.leaves.is_empty()) else { return };
    let dirty: HashSet<Entity> = moved.iter().map(ChildOf::parent).collect();
    for root in dirty {
        let Ok((children, culled)) = roots.get_mut(root) else { continue };
        let corners = children.iter().filter_map(|&child| sections.get(child).ok()).flat_map(|(transform, aabb, sprite)| {
            let (center, half) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
            let any_facing = (half * transform.scale).length();
            (0..8).map(move |i| {
                let sign = Vec3::new([-1.0, 1.0][i & 1], [-1.0, 1.0][(i >> 1) & 1], [-1.0, 1.0][i >> 2]);
                from_bevy(if sprite { transform.translation + sign * any_facing } else { transform.transform_point(center + sign * half) })
            })
        });
        let Some((min, max)) = bounds(corners) else { continue };
        if culled.as_ref().is_some_and(|zone| zone.min == min && zone.max == max) {
            continue;
        }
        let zone = ZoneCulled { zones: p2_collision::box_zones(model, min.into(), max.into()), min, max, only_from: None };
        match culled {
            Some(mut culled) => *culled = zone,
            None => {
                commands.entity(root).insert(zone);
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
            let [x, y, z] = from_bevy(normal);
            Plane::from([x, y, z, -(half.d() + normal.dot(origin))])
        })
        .collect()
}

/// Scenes without BSP portals, and a view outside the world, show everything. While the modern sun casts ([`SunShadow`]),
/// a dynamic object with a sun caster mesh ([`CASTER_LAYER`]) within the sun's last cascade stays shown:
/// `Visibility::Hidden` takes a mesh out of the shadow pass too, so its shadow popped in and out with the view.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn zone_visibility(
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    cameras: Query<(&Transform, &Projection), With<SceneView>>,
    suns: Query<&CascadeShadowConfig, With<SunShadow>>,
    layers: Query<&RenderLayers>,
    mut culled: Query<(Entity, &ZoneCulled, &mut Visibility, Option<&Children>)>,
    mut shadow_only: ResMut<ShadowOnly>,
    mut world_seen: ResMut<WorldSeenZones>,
    mut walk: bevy::ecs::system::Local<VisibleZones>,
) {
    shadow_only.0.clear();
    let model = collision.model();
    let view_zone = model.as_ref().zip(cameras.iter().next()).map(|(model, (transform, _))| {
        p2_collision::point_zone(model, from_bevy(transform.translation - shown.origin).into())
    });
    let mut eye_seen = None;
    let all = VisibleZones::default();
    let seen = match (&shown.portals, cameras.iter().next(), view_zone) {
        (Some(portals), Some((transform, projection)), Some(zone)) => {
            let eye = from_bevy(transform.translation - shown.origin);
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
    for (root, zone, mut visibility, children) in &mut culled {
        let forced = forced_shown(zone.only_from.as_deref(), view_zone);
        let seen = forced && zone.zones.iter().any(|&z| seen.sees(z, zone.min.into(), zone.max.into()));
        let drawn = seen
            || (forced
                && sun.is_some_and(|(eye, reach)| sun_reaches(eye, reach, zone))
                && children.is_some_and(|children| children.iter().any(|&child| layers.get(child).is_ok_and(|l| l.intersects(&caster)))));
        if drawn && !seen {
            shadow_only.0.insert(root);
        }
        visibility.set_if_neq(if drawn { Visibility::Inherited } else { Visibility::Hidden });
    }
}

pub(super) fn forced_shown(only_from: Option<&[u8]>, view_zone: Option<u8>) -> bool {
    only_from.is_none_or(|zones| view_zone.is_none_or(|view| zones.contains(&view)))
}

pub(super) fn sun_reaches(eye: [f32; 3], reach: f32, zone: &ZoneCulled) -> bool {
    let gap = |k: usize| eye[k].max(zone.min[k]).min(zone.max[k]) - eye[k];
    gap(0).powi(2) + gap(1).powi(2) + gap(2).powi(2) <= reach * reach
}
