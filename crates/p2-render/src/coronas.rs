//! Retail draws a quad of `Skins[0]` at each `bCorona` light the eye sees, after the scene:
//! additive, unlit, unfogged, with no Z test or write.

use bevy::app::{App, Plugin, Update};
use bevy::asset::{AssetId, Assets, Handle};
use bevy::camera::visibility::{NoFrustumCulling, Visibility};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::entity::EntityHashMap;
use bevy::ecs::prelude::{Add, ChildOf, Commands, Component, Entity, On, Query, Remove, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::Local;
use bevy::image::Image;
use bevy::math::{Mat3, Quat, Vec3};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::MeshMaterial3d;
use bevy::time::{Real, Time};
use bevy::transform::components::Transform;
use p2_bevy_assets::{to_bevy, unreal_transform};
use p2_math::{Rotator, Vector};
use p2_render_cpu::corona::{Coronas, corona_color, fade_delta, non_square, non_square_scale, size_factor, within_light_radius};
use rustc_hash::FxHashMap;
use crate::components::SceneView;
use crate::level_collision::LevelCollision;
use crate::particle_material::ParticleMaterial;
use crate::scene::{LayeredByZone, ZoneCulled};
use crate::sprites::sprite_quad;
use crate::translucent_order::TranslucentDraw;

#[derive(Component, Clone)]
pub struct CoronaLook {
    pub image: Handle<Image>,
    pub origin: Vec3,
    pub location: Vector,
    pub rotation: Rotator,
    pub size: f32,
    pub draw_scale3d: [f32; 3],
    pub hue: u8,
    pub saturation: u8,
    pub sizes: Option<(f32, f32)>,
    pub light_radius: f32,
}

#[derive(Component, Clone, Copy)]
pub struct CoronaPlace {
    pub gone: bool,
    pub location: Vector,
    pub rotation: Rotator,
    pub blocked: bool,
}

#[derive(Component)]
struct CoronaColor([f32; 3]);

/// Emptied whenever a corona goes, so the materials of a map that was left do not stay alive.
#[derive(Resource, Default)]
struct CoronaMaterials(FxHashMap<AssetId<Image>, Handle<ParticleMaterial>>);

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoronaSystems;

pub struct CoronaPlugin;

impl Plugin for CoronaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CoronaMaterials>()
            .add_observer(add_corona_quad)
            .add_observer(forget_corona_materials)
            .configure_sets(Update, CoronaSystems.after(crate::sprites::SpriteSystems))
            .add_systems(Update, draw_coronas.in_set(CoronaSystems));
    }
}

fn add_corona_quad(
    add: On<Add<CoronaLook>>,
    mut commands: Commands,
    looks: Query<&CoronaLook>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    mut cache: ResMut<CoronaMaterials>,
) {
    let entity = add.event().entity;
    let Ok(look) = looks.get(entity) else { return };
    let material = cache.0.entry(look.image.id()).or_insert_with(|| materials.add(ParticleMaterial::corona(look.image.clone()))).clone();
    commands.entity(entity).insert((Mesh3d(meshes.add(sprite_quad())), MeshMaterial3d(material), NoFrustumCulling, TranslucentDraw::AfterScene, CoronaColor([1.0; 3])));
}

fn forget_corona_materials(_: On<Remove<CoronaLook>>, mut cache: ResMut<CoronaMaterials>) {
    cache.0.clear();
}

/// The fade steps by 3 × the real seconds since the last frame. Lit ones face the view, or lie on the light's axes
/// when `DrawScale3D` X ≠ Y.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn draw_coronas(
    mut commands: Commands,
    collision: Res<LevelCollision>,
    time: Res<Time<Real>>,
    views: Query<&SceneView>,
    mut quads: Query<(Entity, &CoronaLook, &CoronaPlace, &mut CoronaColor, &Mesh3d, &mut Transform, &mut Visibility, &ChildOf)>,
    zones: Query<&Visibility, (With<ZoneCulled>, Without<CoronaLook>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut coronas: Local<Coronas<Entity>>,
    mut lit: Local<EntityHashMap<f32>>,
) {
    let Some(view) = views.iter().next() else { return };
    let model = collision.model();
    let model = model.filter(|model| !model.leaves.is_empty());
    for (_, look, .., parent) in &quads {
        if let Some(model) = model
            && zones.get(parent.parent()).is_err()
        {
            let at: [f32; 3] = look.location.into();
            commands.entity(parent.parent()).insert((ZoneCulled { zones: p2_collision::box_zones(model, at.into(), at.into()), min: at, max: at, only_from: None }, LayeredByZone));
        }
    }
    let in_reach = |look: &CoronaLook, place: &CoronaPlace| {
        let eye = view.position - look.origin;
        within_light_radius(eye.distance(Vec3::from(to_bevy(place.location))), look.light_radius)
    };
    let zone_seen = |parent: &ChildOf| zones.get(parent.parent()).is_ok_and(|shown| *shown != Visibility::Hidden);
    let seen = quads.iter().filter(|(_, look, place, .., parent)| !place.gone && !place.blocked && in_reach(look, place) && zone_seen(parent)).map(|(entity, ..)| entity);
    coronas.step(fade_delta(time.delta_secs()), seen, |_| true);
    lit.clear();
    lit.extend(coronas.lit());
    for (entity, look, place, mut drawn, mesh, mut transform, mut shown, _) in &mut quads {
        let Some(&bright) = lit.get(&entity) else {
            shown.set_if_neq(Visibility::Hidden);
            continue;
        };
        let translation = Vec3::from(to_bevy(place.location));
        let factor = size_factor((view.position - look.origin).distance(translation), look.sizes);
        // The quad's +X is retail's right, its +Y retail's −up (where the texture's top is, as for sprites).
        let (facing, scale) = if non_square(look.draw_scale3d) {
            let axes = unreal_transform(place.location, place.rotation, Vector::from([1.0; 3]), Vector::from([0.0; 3])).rotation;
            let (x, y) = (axes * Vec3::X, -(axes * Vec3::Z));
            let front = view.rotation * Vec3::NEG_Z;
            (Quat::from_mat3(&Mat3::from_cols(x, y, x.cross(y))), non_square_scale(front.to_array().into(), x.to_array().into(), y.to_array().into()))
        } else {
            (view.rotation, 1.0)
        };
        let size = look.size * factor;
        let faced = Transform { translation, rotation: facing, scale: Vec3::new(size * look.draw_scale3d[0], size * look.draw_scale3d[1], 1.0) };
        let color = corona_color(look.hue, look.saturation, bright).map(|c| c * scale);
        if *transform != faced {
            *transform = faced;
        }
        if drawn.0 != color
            && let Some(mut mesh) = meshes.get_mut(&mesh.0)
        {
            p2_host::frame_times::mesh_written(p2_host::frame_times::MeshWrite::Corona);
            let [r, g, b] = color;
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[r, g, b, 1.0]; 4]);
            drawn.0 = color;
        }
        shown.set_if_neq(Visibility::Inherited);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    #[test]
    fn a_blocked_corona_fades_out_and_one_in_reach_fades_in() {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(100)))
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<ParticleMaterial>>()
            .init_resource::<LevelCollision>()
            .add_plugins(CoronaPlugin);
        app.world_mut().spawn(SceneView::default());
        let parent = app.world_mut().spawn((ZoneCulled { zones: Vec::new(), min: [0.0; 3], max: [0.0; 3], only_from: None }, Visibility::Inherited)).id();
        let location = Vector::from([100.0, 0.0, 0.0]);
        let rotation = Rotator::from([0; 3]);
        let look = CoronaLook { image: Handle::default(), origin: Vec3::ZERO, location, rotation, size: 1024.0, draw_scale3d: [1.0; 3], hue: 0, saturation: 255, sizes: None, light_radius: 0.0 };
        let place = CoronaPlace { gone: false, location, rotation, blocked: false };
        let [blocked, open] = [(); 2].map(|_| app.world_mut().spawn((look.clone(), place, Transform::default(), Visibility::Hidden, ChildOf(parent))).id());
        let shown = |app: &App, corona: Entity| *app.world().entity(corona).get::<Visibility>().expect("a visibility");
        for _ in 0..5 {
            app.update();
        }
        assert_eq!([shown(&app, blocked), shown(&app, open)], [Visibility::Inherited; 2]);
        app.world_mut().entity_mut(blocked).insert(CoronaPlace { blocked: true, ..place });
        for _ in 0..6 {
            app.update();
        }
        assert_eq!([shown(&app, blocked), shown(&app, open)], [Visibility::Hidden, Visibility::Inherited]);
    }
}
