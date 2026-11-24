//! Retail draws a quad of `Skins[0]` at each `bCorona` light the eye sees, after the scene:
//! additive, unlit, unfogged, with no Z test or write.

use bevy::app::{App, Plugin, Update};
use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{NoFrustumCulling, Visibility};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::entity::EntityHashMap;
use bevy::ecs::prelude::{Add, ChildOf, Commands, Component, Entity, On, Query, Remove, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::Local;
use bevy::image::Image;
use bevy::math::{Mat3, Quat, Vec3};
use bevy::mesh::{Mesh, Mesh3d, MeshTag};
use bevy::pbr::MeshMaterial3d;
use bevy::render::storage::ShaderBuffer;
use bevy::time::{Real, Time};
use bevy::transform::components::Transform;
use p2_bevy_assets::{to_bevy, unreal_transform};
use p2_math::{Rotator, Vector};
use p2_render_cpu::corona::{Coronas, corona_color, fade_delta, non_square, non_square_scale, size_factor, within_light_radius};
use rustc_hash::FxHashMap;
use crate::components::SceneView;
use crate::level_collision::LevelCollision;
use crate::particle_frame::DrawnParticles;
use crate::particle_material::{CORONA_COLORS, ParticleMaterial, RECORD_WORDS};
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

/// Emptied when the map changes, so the materials of a map that was left do not stay alive.
#[derive(Resource, Default)]
struct CoronaMaterials {
    level: u32,
    materials: FxHashMap<AssetId<Image>, Handle<ParticleMaterial>>,
}

const MIN_COLOR_SLOTS: usize = 16;

/// The records of `CORONA_COLORS`, one slot per corona. Grows only in steps: a new byte size makes a new GPU buffer.
#[derive(Resource, Default)]
struct CoronaColors {
    records: Vec<[u32; RECORD_WORDS]>,
    free: Vec<usize>,
    dirty: bool,
}

impl CoronaColors {
    fn take_slot(&mut self) -> usize {
        let slot = self.free.pop().unwrap_or_else(|| {
            let slot = self.records.len();
            self.records.resize((slot * 2).max(MIN_COLOR_SLOTS), [0; RECORD_WORDS]);
            self.free.extend((slot + 1..self.records.len()).rev());
            slot
        });
        self.set(slot, [1.0; 3]);
        slot
    }

    fn set(&mut self, slot: usize, color: [f32; 3]) {
        if let Some(record) = self.records.get_mut(slot) {
            record[0..3].copy_from_slice(&color.map(f32::to_bits));
            self.dirty = true;
        }
    }

    fn upload(&mut self, buffers: &mut Assets<ShaderBuffer>) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        if !buffers.contains(CORONA_COLORS.id()) {
            if let Err(error) = buffers.insert(CORONA_COLORS.id(), ShaderBuffer::new(self.records.clone(), RenderAssetUsages::default())) {
                bevy::log::error!("corona colour buffer: {error}");
            }
            return;
        }
        if let Some(mut buffer) = buffers.get_mut(CORONA_COLORS.id()) {
            buffer.clear();
            buffer.extend_from_slice(&self.records);
        }
    }
}

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoronaSystems;

pub struct CoronaPlugin;

impl Plugin for CoronaPlugin {
    fn build(&self, app: &mut App) {
        // Before any corona's material is prepared: a material bound to a buffer not yet made gets Bevy's one-byte
        // stand-in, which no storage binding takes.
        if let Some(mut buffers) = app.world_mut().get_resource_mut::<Assets<ShaderBuffer>>()
            && let Err(error) = buffers.insert(CORONA_COLORS.id(), ShaderBuffer::new(vec![[0u32; RECORD_WORDS]; MIN_COLOR_SLOTS], RenderAssetUsages::default()))
        {
            bevy::log::error!("corona colour buffer: {error}");
        }
        app.init_resource::<CoronaMaterials>()
            .init_resource::<CoronaColors>()
            .add_observer(add_corona_quad)
            .add_observer(free_corona_slot)
            .configure_sets(Update, CoronaSystems.after(crate::sprites::SpriteSystems))
            .add_systems(Update, (draw_coronas, forget_corona_materials).in_set(CoronaSystems));
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy observer's parameters")]
fn add_corona_quad(
    add: On<Add<CoronaLook>>,
    mut commands: Commands,
    looks: Query<&CoronaLook>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    mut cache: ResMut<CoronaMaterials>,
    mut colors: ResMut<CoronaColors>,
    mut quad: Local<Option<Handle<Mesh>>>,
) {
    let entity = add.event().entity;
    let Ok(look) = looks.get(entity) else { return };
    let quad = quad.get_or_insert_with(|| meshes.add(sprite_quad())).clone();
    let material = cache.materials.entry(look.image.id()).or_insert_with(|| materials.add(ParticleMaterial::corona(look.image.clone()))).clone();
    let tag = MeshTag::new(u32::try_from(colors.take_slot()).unwrap_or(u32::MAX));
    commands.entity(entity).insert((Mesh3d(quad), tag, MeshMaterial3d(material), NoFrustumCulling, TranslucentDraw::AfterScene, CoronaColor([1.0; 3])));
}

fn free_corona_slot(remove: On<Remove<CoronaLook>>, tags: Query<&MeshTag>, mut colors: ResMut<CoronaColors>) {
    if let Ok(tag) = tags.get(remove.event().entity)
        && let Ok(slot) = usize::try_from(tag.value)
    {
        colors.free.push(slot);
    }
}

fn forget_corona_materials(drawn: Option<Res<DrawnParticles>>, mut cache: ResMut<CoronaMaterials>) {
    if let Some(drawn) = drawn
        && cache.level != drawn.level
    {
        cache.level = drawn.level;
        cache.materials.clear();
    }
}

/// The fade steps by 3 × the real seconds since the last frame. Lit ones face the view, or lie on the light's axes
/// when `DrawScale3D` X ≠ Y.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn draw_coronas(
    mut commands: Commands,
    collision: Res<LevelCollision>,
    time: Res<Time<Real>>,
    views: Query<&SceneView>,
    mut quads: Query<(Entity, &CoronaLook, &CoronaPlace, &mut CoronaColor, &MeshTag, &mut Transform, &mut Visibility, &ChildOf)>,
    zones: Query<&Visibility, (With<ZoneCulled>, Without<CoronaLook>)>,
    mut colors: ResMut<CoronaColors>,
    buffers: Option<ResMut<Assets<ShaderBuffer>>>,
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
    for (entity, look, place, mut drawn, tag, mut transform, mut shown, _) in &mut quads {
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
            && let Ok(slot) = usize::try_from(tag.value)
        {
            colors.set(slot, color);
            drawn.0 = color;
        }
        shown.set_if_neq(Visibility::Inherited);
    }
    if let Some(mut buffers) = buffers {
        colors.upload(&mut buffers);
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
            .init_resource::<Assets<ShaderBuffer>>()
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
        let world = app.world();
        let uploaded = world.resource::<Assets<ShaderBuffer>>().get(CORONA_COLORS.id()).and_then(|buffer| buffer.cast_slice::<[u32; RECORD_WORDS]>()).expect("the colour buffer");
        assert_eq!(uploaded, world.resource::<CoronaColors>().records.as_slice());
        let tags = [blocked, open].map(|corona| world.entity(corona).get::<MeshTag>().expect("a slot").value as usize);
        assert_ne!(tags[0], tags[1]);
        let color = world.entity(open).get::<CoronaColor>().expect("a colour").0;
        assert_eq!(uploaded[tags[1]][0..3], color.map(f32::to_bits));
        app.world_mut().entity_mut(blocked).despawn();
        assert_eq!(app.world().resource::<CoronaColors>().free.last(), Some(&tags[0]));
    }
}
