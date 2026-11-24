use bevy::app::{App, Plugin, PostUpdate};
use bevy::camera::primitives::{Aabb, CascadesFrusta, Frustum, Sphere};
use bevy::camera::visibility::{
    InheritedVisibility, NoCpuCulling, NoFrustumCulling, RenderLayers, ViewVisibility, VisibilityRange, VisibilitySystems,
};
use bevy::camera::{Camera, Camera3d, ShadowLodOrigin};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{Commands, Entity, Has, Local, Or, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::light::{DirectionalLight, NotShadowCaster, SimulationLightSystems};
use bevy::math::Vec3A;
use bevy::mesh::Mesh3d;
use bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport;
use bevy::render::{ExtractSchedule, MainWorld, RenderApp};
use bevy::transform::components::GlobalTransform;

use crate::components::{FollowActor, FollowBone};

pub struct MoverSightPlugin;

impl Plugin for MoverSightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MoversSeen>().init_resource::<GpuCulls>().add_systems(
            PostUpdate,
            (
                tag_movers.before(VisibilitySystems::CheckVisibility),
                see_movers.after(VisibilitySystems::CheckVisibility).after(SimulationLightSystems::UpdateLightFrusta),
            ),
        );
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(ExtractSchedule, learn_gpu_culling);
        }
    }
}

/// The movers' meshes an active 3D camera drew this frame.
#[derive(Resource, Default)]
pub struct MoversSeen {
    /// Sorted, as the engine binary-searches it.
    pub drawn: Vec<Entity>,
}

/// Whether the device culls on the GPU; Bevy knows it only in the render world, from its first render frame.
#[derive(Resource, Default, PartialEq)]
struct GpuCulls(bool);

fn learn_gpu_culling(support: Option<Res<GpuPreprocessingSupport>>, mut main_world: ResMut<MainWorld>) {
    let culls = GpuCulls(support.is_some_and(|support| support.is_culling_supported()));
    if let Some(mut known) = main_world.get_resource_mut::<GpuCulls>() {
        known.set_if_neq(culls);
    }
}

fn tag_movers(
    mut commands: Commands,
    culls: Res<GpuCulls>,
    untagged: Query<Entity, (With<Mesh3d>, Or<(With<FollowActor>, With<FollowBone>)>, Without<NoCpuCulling>)>,
) {
    if !culls.0 {
        return;
    }
    for entity in &untagged {
        commands.entity(entity).insert(NoCpuCulling);
    }
}

struct Eye {
    frustum: Frustum,
    layers: RenderLayers,
    at: Vec3A,
    no_cpu_culling: bool,
}

struct Cascade {
    frustum: Frustum,
    layers: RenderLayers,
    /// The view the cascade belongs to, where its visibility ranges are measured from.
    eye: Option<Vec3A>,
}

struct MoverBounds<'a> {
    inherited: InheritedVisibility,
    layers: &'a RenderLayers,
    aabb: Option<&'a Aabb>,
    transform: &'a GlobalTransform,
    no_frustum_culling: bool,
    range: Option<&'a VisibilityRange>,
    casts: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sight {
    Drawn,
    Casting,
    Neither,
}

/// Bevy's own camera and cascade tests, with their flags.
fn sight(mover: &MoverBounds, eyes: &[Eye], cascades: &[Cascade]) -> Sight {
    if !mover.inherited.get() {
        return Sight::Neither;
    }
    let world_from_local = mover.transform.affine();
    let origin = match (mover.range.map(|range| range.use_aabb), mover.aabb) {
        (Some(true), Some(aabb)) => world_from_local.transform_point3a(aabb.center),
        _ => mover.transform.translation_vec3a(),
    };
    let in_range = |eye: Option<Vec3A>| mover.range.is_none_or(|range| eye.is_some_and(|eye| range.is_visible_at_all((eye - origin).length())));
    let drawn = eyes.iter().any(|eye| {
        eye.layers.intersects(mover.layers)
            && in_range(Some(eye.at))
            && (mover.no_frustum_culling
                || eye.no_cpu_culling
                || mover.aabb.is_none_or(|aabb| {
                    let sphere = Sphere { center: world_from_local.transform_point3a(aabb.center), radius: mover.transform.radius_vec3a(aabb.half_extents) };
                    eye.frustum.intersects_sphere(&sphere, false) && eye.frustum.intersects_obb(aabb, &world_from_local, true, false)
                }))
    });
    if drawn {
        return Sight::Drawn;
    }
    let casting = mover.casts
        && cascades.iter().any(|cascade| {
            cascade.layers.intersects(mover.layers)
                && in_range(cascade.eye)
                && (mover.no_frustum_culling || mover.aabb.is_none_or(|aabb| cascade.frustum.intersects_obb(aabb, &world_from_local, false, true)))
        });
    if casting { Sight::Casting } else { Sight::Neither }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn see_movers(
    culls: Res<GpuCulls>,
    mut seen: ResMut<MoversSeen>,
    mut movers: Query<
        (
            Entity,
            &InheritedVisibility,
            &mut ViewVisibility,
            Option<&RenderLayers>,
            Option<&Aabb>,
            &GlobalTransform,
            Has<NoFrustumCulling>,
            Option<&VisibilityRange>,
            Has<NotShadowCaster>,
        ),
        (With<NoCpuCulling>, With<Mesh3d>, Or<(With<FollowActor>, With<FollowBone>)>),
    >,
    cameras: Query<(&Camera, &Frustum, Option<&RenderLayers>, &GlobalTransform, Has<NoCpuCulling>), With<Camera3d>>,
    suns: Query<(&DirectionalLight, &CascadesFrusta, Option<&RenderLayers>, &ViewVisibility), Without<Mesh3d>>,
    views: Query<&GlobalTransform, Or<(With<Camera>, With<ShadowLodOrigin>)>>,
    mut eyes: Local<Vec<Eye>>,
    mut cascades: Local<Vec<Cascade>>,
) {
    seen.drawn.clear();
    if !culls.0 {
        return;
    }
    eyes.clear();
    eyes.extend(cameras.iter().filter(|(camera, ..)| camera.is_active).map(|(_, frustum, layers, transform, no_cpu_culling)| Eye {
        frustum: *frustum,
        layers: layers.cloned().unwrap_or_default(),
        at: transform.translation_vec3a(),
        no_cpu_culling,
    }));
    cascades.clear();
    for (sun, frusta, layers, sun_visibility) in &suns {
        if !sun.shadow_maps_enabled || !sun_visibility.get() {
            continue;
        }
        let layers = layers.cloned().unwrap_or_default();
        for (view, view_frusta) in &frusta.frusta {
            let eye = views.get(*view).ok().map(GlobalTransform::translation_vec3a);
            cascades.extend(view_frusta.iter().map(|frustum| Cascade { frustum: *frustum, layers: layers.clone(), eye }));
        }
    }
    let default_layers = RenderLayers::default();
    for (entity, inherited, mut view_visibility, layers, aabb, transform, no_frustum_culling, range, not_caster) in &mut movers {
        let mover = MoverBounds {
            inherited: *inherited,
            layers: layers.unwrap_or(&default_layers),
            aabb,
            transform,
            no_frustum_culling,
            range,
            casts: !not_caster,
        };
        let sight = sight(&mover, &eyes, &cascades);
        view_visibility.set_if_neq(if sight == Sight::Neither { ViewVisibility::HIDDEN } else { ViewVisibility::VISIBLE });
        if sight == Sight::Drawn {
            seen.drawn.push(entity);
        }
    }
    seen.drawn.sort_unstable();
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::{PerspectiveProjection, Projection};
    use bevy::math::{Quat, Vec3};
    use bevy::transform::components::Transform;

    const UNIT: Aabb = Aabb { center: Vec3A::ZERO, half_extents: Vec3A::ONE };

    fn frustum_from(transform: Transform) -> Frustum {
        let projection = Projection::Perspective(PerspectiveProjection { near: 1.0, far: 1000.0, ..Default::default() });
        projection.compute_frustum(&GlobalTransform::from(transform))
    }

    fn eye_looking_down_negative_z() -> Eye {
        Eye { frustum: frustum_from(Transform::IDENTITY), layers: RenderLayers::default(), at: Vec3A::ZERO, no_cpu_culling: false }
    }

    fn cascade_looking_down_positive_z() -> Cascade {
        let frustum = frustum_from(Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)));
        Cascade { frustum, layers: RenderLayers::default(), eye: Some(Vec3A::ZERO) }
    }

    fn at(z: f32) -> GlobalTransform {
        GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, z)))
    }

    fn mover<'a>(transform: &'a GlobalTransform, layers: &'a RenderLayers) -> MoverBounds<'a> {
        MoverBounds { inherited: InheritedVisibility::VISIBLE, layers, aabb: Some(&UNIT), transform, no_frustum_culling: false, range: None, casts: true }
    }

    #[test]
    fn a_mover_in_front_of_the_camera_is_drawn_and_one_behind_it_is_not() {
        let (layers, eyes) = (RenderLayers::default(), [eye_looking_down_negative_z()]);
        assert_eq!(sight(&mover(&at(-50.0), &layers), &eyes, &[]), Sight::Drawn);
        assert_eq!(sight(&mover(&at(50.0), &layers), &eyes, &[]), Sight::Neither);
    }

    #[test]
    fn a_mover_past_the_end_of_its_range_is_not_drawn() {
        let (layers, eyes, transform) = (RenderLayers::default(), [eye_looking_down_negative_z()], at(-50.0));
        let near = VisibilityRange::abrupt(0.0, 100.0);
        let far = VisibilityRange::abrupt(0.0, 20.0);
        assert_eq!(sight(&MoverBounds { range: Some(&near), ..mover(&transform, &layers) }, &eyes, &[]), Sight::Drawn);
        assert_eq!(sight(&MoverBounds { range: Some(&far), ..mover(&transform, &layers) }, &eyes, &[]), Sight::Neither);
    }

    #[test]
    fn a_mover_outside_the_camera_but_inside_a_cascade_casts_unless_it_casts_no_shadow() {
        let (layers, eyes, cascades, transform) = (RenderLayers::default(), [eye_looking_down_negative_z()], [cascade_looking_down_positive_z()], at(50.0));
        assert_eq!(sight(&mover(&transform, &layers), &eyes, &cascades), Sight::Casting);
        assert_eq!(sight(&MoverBounds { casts: false, ..mover(&transform, &layers) }, &eyes, &cascades), Sight::Neither);
    }

    #[test]
    fn a_mover_whose_layers_meet_no_camera_or_cascade_is_neither() {
        let (layers, eyes, cascades) = (RenderLayers::layer(3), [eye_looking_down_negative_z()], [cascade_looking_down_positive_z()]);
        assert_eq!(sight(&mover(&at(-50.0), &layers), &eyes, &cascades), Sight::Neither);
        assert_eq!(sight(&mover(&at(50.0), &layers), &eyes, &cascades), Sight::Neither);
    }

    #[test]
    fn a_hidden_mover_is_neither_wherever_it_stands() {
        let (layers, eyes, cascades, transform) = (RenderLayers::default(), [eye_looking_down_negative_z()], [cascade_looking_down_positive_z()], at(-50.0));
        assert_eq!(sight(&MoverBounds { inherited: InheritedVisibility::HIDDEN, ..mover(&transform, &layers) }, &eyes, &cascades), Sight::Neither);
    }
}
