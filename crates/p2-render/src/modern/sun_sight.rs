use bevy::camera::primitives::{Aabb, CascadesFrusta, Frustum};
use bevy::camera::visibility::{
    CascadesVisibleEntities, InheritedVisibility, NoCpuCulling, NoFrustumCulling, RenderLayers, SetViewVisibility, ViewVisibility, VisibilityRange,
    VisibleEntityRanges, VisibleMeshEntities,
};
use bevy::ecs::prelude::{Entity, Has, Or, ParamSet, Query, Res, With, Without};
use bevy::ecs::query::QueryFilter;
use bevy::light::{DirectionalLight, NotShadowCaster, SpotLight};
use bevy::mesh::Mesh3d;
use bevy::transform::components::GlobalTransform;

use crate::components::{FollowActor, FollowBone, MOVER_LAYER, SUN_VIEW_LAYER, SunShadow};

type Caster = (
    Entity,
    &'static InheritedVisibility,
    &'static mut ViewVisibility,
    Option<&'static RenderLayers>,
    Option<&'static Aabb>,
    Option<&'static GlobalTransform>,
    Has<VisibilityRange>,
    Has<NoFrustumCulling>,
);
type AnyCaster = (Without<NotShadowCaster>, Without<DirectionalLight>, Without<NoCpuCulling>, With<Mesh3d>);
type MoverCaster = (AnyCaster, Or<(With<FollowActor>, With<FollowBone>)>);

pub(super) fn see_sun_casters(
    mut suns: Query<
        (&DirectionalLight, &CascadesFrusta, &mut CascadesVisibleEntities, Option<&RenderLayers>, &ViewVisibility, Has<SunShadow>),
        Without<SpotLight>,
    >,
    mut casters: ParamSet<(Query<Caster, AnyCaster>, Query<Caster, MoverCaster>)>,
    ranges: Option<Res<VisibleEntityRanges>>,
) {
    let ranges = ranges.as_deref();
    for (sun, frusta, mut found, layers, sun_visibility, ours) in &mut suns {
        found.entities.retain(|view, _| frusta.frusta.contains_key(view));
        for (view, view_frusta) in &frusta.frusta {
            found.entities.entry(*view).or_default().resize(view_frusta.len(), VisibleMeshEntities::default());
        }
        if !sun.shadow_maps_enabled || !sun_visibility.get() {
            found.entities.clear();
            continue;
        }
        let layers = layers.unwrap_or_default();
        for (view, view_frusta) in &frusta.frusta {
            let Some(cascades) = found.entities.get_mut(view) else { continue };
            for cascade in cascades.iter_mut() {
                cascade.entities.clear();
            }
            if ours && *layers == RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER]) {
                cast_into(&mut casters.p1(), *view, view_frusta, layers, ranges, cascades);
            } else {
                cast_into(&mut casters.p0(), *view, view_frusta, layers, ranges, cascades);
            }
            for cascade in cascades.iter_mut() {
                cascade.shrink();
                cascade.entities.sort_unstable();
            }
        }
    }
}

fn cast_into<F: QueryFilter>(
    casters: &mut Query<Caster, F>,
    view: Entity,
    frusta: &[Frustum],
    layers: &RenderLayers,
    ranges: Option<&VisibleEntityRanges>,
    cascades: &mut [VisibleMeshEntities],
) {
    for (entity, inherited, mut view_visibility, own_layers, aabb, transform, ranged, unculled) in casters.iter_mut() {
        if !inherited.get() || !layers.intersects(own_layers.unwrap_or_default()) {
            continue;
        }
        if ranged && ranges.is_some_and(|ranges| !ranges.entity_is_in_range_of_view(entity, view)) {
            continue;
        }
        let bounds = aabb.zip(transform);
        let mut visible = bounds.is_none();
        for (frustum, cascade) in frusta.iter().zip(cascades.iter_mut()) {
            // No near plane: a caster may stand between the sun and the cascade.
            if bounds.is_none_or(|(aabb, transform)| unculled || frustum.intersects_obb(aabb, &transform.affine(), false, true)) {
                visible = true;
                cascade.entities.push(entity);
            }
        }
        if visible {
            view_visibility.set_visible();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{CASTER_LAYER, STATIC_CASTER_LAYER};
    use bevy::camera::{PerspectiveProjection, Projection};
    use bevy::ecs::system::RunSystemOnce;
    use bevy::ecs::world::World;
    use bevy::light::check_dir_light_mesh_visibility;
    use bevy::math::{Vec3, Vec3A};
    use bevy::tasks::{ComputeTaskPool, TaskPool};
    use bevy::transform::components::Transform;

    fn frustum(far: f32) -> Frustum {
        let projection = Projection::Perspective(PerspectiveProjection { near: 1.0, far, ..Default::default() });
        projection.compute_frustum(&GlobalTransform::IDENTITY)
    }

    fn mesh_at(world: &mut World, z: f32, layers: RenderLayers) -> Entity {
        let place = GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, z)));
        let bounds = Aabb { center: Vec3A::ZERO, half_extents: Vec3A::ONE };
        world.spawn((Mesh3d::default(), InheritedVisibility::VISIBLE, ViewVisibility::HIDDEN, bounds, place, layers)).id()
    }

    fn level() -> (World, Vec<Entity>) {
        let mut world = World::new();
        let view = world.spawn_empty().id();
        let mut cascades = CascadesFrusta::default();
        cascades.frusta.insert(view, vec![frustum(20.0), frustum(200.0)]);
        let shadowed = DirectionalLight { shadow_maps_enabled: true, ..DirectionalLight::default() };
        let sun_layers = RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER]);
        world.spawn((SunShadow, shadowed, cascades.clone(), CascadesVisibleEntities::default(), sun_layers, ViewVisibility::VISIBLE));
        world.spawn((shadowed, cascades, CascadesVisibleEntities::default(), ViewVisibility::VISIBLE));
        let follow = FollowActor { actor: p2_vm::ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
        let mover = RenderLayers::from_layers(&[0, CASTER_LAYER, MOVER_LAYER]);
        let still = RenderLayers::from_layers(&[0, CASTER_LAYER, STATIC_CASTER_LAYER]);
        let near_mover = mesh_at(&mut world, -10.0, mover.clone());
        world.entity_mut(near_mover).insert(follow);
        let far_mover = mesh_at(&mut world, -100.0, mover.clone());
        world.entity_mut(far_mover).insert(follow);
        let behind_mover = mesh_at(&mut world, 500.0, mover);
        world.entity_mut(behind_mover).insert(follow);
        let floor = mesh_at(&mut world, -10.0, still);
        let plain = mesh_at(&mut world, -50.0, RenderLayers::default());
        (world, vec![near_mover, far_mover, behind_mover, floor, plain])
    }

    fn outcome(world: &mut World, meshes: &[Entity]) -> (Vec<(bool, Vec<Vec<Entity>>)>, Vec<bool>) {
        let mut suns: Vec<_> = world
            .query::<(Has<SunShadow>, &CascadesVisibleEntities)>()
            .iter(world)
            .map(|(ours, found)| {
                let sorted = |entities: &[Entity]| {
                    let mut entities = entities.to_vec();
                    entities.sort_unstable();
                    entities
                };
                (ours, found.entities.values().flat_map(|cascades| cascades.iter().map(|cascade| sorted(&cascade.entities))).collect())
            })
            .collect();
        suns.sort();
        let seen = meshes.iter().map(|&mesh| world.get::<ViewVisibility>(mesh).is_some_and(|visibility| visibility.get())).collect();
        (suns, seen)
    }

    #[test]
    fn the_cascades_see_what_bevy_sees() {
        ComputeTaskPool::get_or_init(TaskPool::default);
        let (mut bevy, meshes) = level();
        bevy.run_system_once(check_dir_light_mesh_visibility).expect("runs");
        let (mut ours, _) = level();
        ours.run_system_once(see_sun_casters).expect("runs");
        let (suns, seen) = outcome(&mut bevy, &meshes);
        assert_eq!(outcome(&mut ours, &meshes), (suns.clone(), seen.clone()));
        let both = { let mut both = vec![meshes[0], meshes[1]]; both.sort_unstable(); both };
        assert_eq!(suns.last(), Some(&(true, vec![vec![meshes[0]], both])));
        assert_eq!(seen, vec![true, true, false, true, true]);
    }
}
