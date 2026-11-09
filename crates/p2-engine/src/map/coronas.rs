//! Retail draws a quad of `Skins[0]` at each `bCorona` light the eye sees, after the scene:
//! additive, unlit, unfogged, with no Z test or write.

use super::*;
use super::sprites::sprite_quad;
use crate::play::Play;
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::math::{Mat3, Quat};
use bevy::time::{Real, Time};
use p2_math::{Rotator, Vector};
use p2_render_cpu::corona::{Coronas, corona_color, fade_delta, non_square, non_square_scale, size_factor, within_light_radius};
use p2_vm::{LoaderRef, Prop, Value};

const CORONA_SORT: f32 = 1.0e6;

const CORONA_SIZE: f32 = 1024.0;

pub struct CoronaActor {
    pub object: ObjectId,
    pub texture: String,
    pub location: Vector,
    pub rotation: Rotator,
    pub draw_scale: f32,
    pub draw_scale3d: [f32; 3],
    pub hue: u8,
    pub saturation: u8,
    pub sizes: Option<(f32, f32)>,
    pub light_radius: f32,
}

fn corona_texture(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<ObjectId> {
    let lit = matches!(vm.store.get(actor, Prop::LightType), Some(Value::Byte(light_type)) if light_type != 0);
    if !vm.store.flag_of(actor, Prop::bCorona) || !lit || vm.store.flag_of(actor, Prop::bDeleteMe) || vm.store.is_deleted(actor) {
        return None;
    }
    let Some(Value::Array(skins)) = vm.store.get(actor, Prop::Skins) else { return None };
    let Some(&Value::Object(Some(texture))) = skins.first() else { return None };
    // Textures are mostly stubs (known by reference, no class loaded): `is_a` alone rejected every corona.
    if !vm.store.is_a(texture, "Texture") && !vm.stub_is_a(texture, "Texture") {
        return None;
    }
    Some(texture)
}

pub fn is_corona(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> bool {
    corona_texture(vm, actor).is_some()
}

pub fn corona_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<CoronaActor> {
    let texture = corona_texture(vm, actor)?;
    let float = |name: &str, default: f32| match vm.store.get(actor, name) {
        Some(Value::Float(value)) => value,
        _ => default,
    };
    let (location, rotation) = location_rotation(vm, actor);
    Some(CoronaActor {
        object: actor,
        texture: vm.store.object(texture).path.clone(),
        location,
        rotation,
        draw_scale: float("DrawScale", 1.0),
        draw_scale3d: vm.store.vector(actor, "DrawScale3D").into(),
        hue: vm.store.byte_of(actor, "LightHue"),
        saturation: vm.store.byte_of(actor, "LightSaturation"),
        sizes: vm.store.is_a(actor, "Light").then(|| (float("MinCoronaSize", 0.0), float("MaxCoronaSize", 0.0))),
        light_radius: float("LightRadius", 0.0),
    })
}

#[derive(bevy::ecs::component::Component)]
pub struct CoronaQuad {
    corona: CoronaActor,
    origin: Vec3,
    color: [f32; 3],
}

impl Session {
    pub(super) fn add_coronas(&mut self, coronas: Vec<CoronaActor>, origin: Vec3, t: &mut SceneTargets) {
        if coronas.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for corona in coronas {
            let (images, particle_materials) = (&mut t.assets.images, &mut t.assets.particle_materials);
            let textures = &mut self.actor_textures;
            let material = self.corona_materials.entry(corona.texture.clone()).or_insert_with_key(|path| {
                let texture = textures.entry(path.clone()).or_insert_with(|| ActorTexture::read(game, path, |image| images.add(image))).as_ref()?;
                let material = ParticleMaterial { sort_bias: CORONA_SORT, ..ParticleMaterial::corona(texture.image.clone()) };
                Some(particle_materials.add(material))
            });
            let Some(material) = material.clone() else {
                bevy::log::warn!("corona {:?}: texture {} not loaded, no quad", corona.object, corona.texture);
                continue;
            };
            let parent = t.commands.spawn((ActorModel(corona.object), Transform::from_translation(origin), Visibility::default())).id();
            t.commands.spawn((
                Mesh3d(t.assets.meshes.add(sprite_quad())),
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
                NoFrustumCulling,
                CoronaQuad { corona, origin, color: [1.0; 3] },
                ChildOf(parent),
            ));
        }
    }
}

/// The fade steps by 3 × the real seconds since the last frame. The VM's trace hits level geometry
/// and movers, not pawns. Lit ones face the view, or lie on the light's axes when `DrawScale3D` X ≠ Y.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub fn draw_coronas(
    mut commands: Commands,
    collision: Res<LevelCollision>,
    time: Res<Time<Real>>,
    play: Res<Play>,
    views: Query<&SceneView>,
    mut quads: Query<(Entity, &mut CoronaQuad, &Mesh3d, &mut Transform, &mut Visibility, &ChildOf)>,
    zones: Query<&Visibility, (With<ZoneCulled>, Without<CoronaQuad>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut coronas: bevy::ecs::system::Local<Coronas<Entity>>,
    mut lit: bevy::ecs::system::Local<bevy::ecs::entity::EntityHashMap<f32>>,
) {
    let Some(view) = views.iter().next() else { return };
    let world = play.world();
    let placed = |quad: &CoronaQuad| match world.and_then(|world| world.actor(quad.corona.object)) {
        Some(frame) => (frame.gone, frame.location, frame.rotation),
        None => (false, quad.corona.location, quad.corona.rotation),
    };
    let blocked = |quad: &CoronaQuad| world.is_some_and(|world| world.corona_blocked(quad.corona.object));
    let model = collision.model();
    let model = model.filter(|model| !model.leaves.is_empty());
    for (_, quad, .., parent) in &quads {
        if let Some(model) = model
            && zones.get(parent.parent()).is_err()
        {
            let at: [f32; 3] = quad.corona.location.into();
            commands.entity(parent.parent()).insert(ZoneCulled { zones: p2_collision::box_zones(model, at.into(), at.into()), min: at, max: at, only_from: None });
        }
    }
    let in_reach = |quad: &CoronaQuad| {
        let eye = view.position - quad.origin;
        within_light_radius(eye.distance(Vec3::from(to_bevy(placed(quad).1))), quad.corona.light_radius)
    };
    let zone_seen = |parent: &ChildOf| zones.get(parent.parent()).is_ok_and(|shown| *shown != Visibility::Hidden);
    let seen = quads.iter().filter(|(_, quad, .., parent)| !placed(quad).0 && !blocked(quad) && in_reach(quad) && zone_seen(parent)).map(|(entity, ..)| entity);
    coronas.step(fade_delta(time.delta_secs()), seen, |_| true);
    lit.clear();
    lit.extend(coronas.lit());
    for (entity, mut quad, mesh, mut transform, mut shown, _) in &mut quads {
        let Some(&bright) = lit.get(&entity) else {
            shown.set_if_neq(Visibility::Hidden);
            continue;
        };
        let (_, location, rotation) = placed(&quad);
        let corona = &quad.corona;
        let translation = Vec3::from(to_bevy(location));
        let factor = size_factor((view.position - quad.origin).distance(translation), corona.sizes);
        // The quad's +X is retail's right, its +Y retail's −up (where the texture's top is, as for sprites).
        let (facing, scale) = if non_square(corona.draw_scale3d) {
            let axes = unreal_transform(location, rotation, Vector::from([1.0; 3]), Vector::from([0.0; 3])).rotation;
            let (x, y) = (axes * Vec3::X, -(axes * Vec3::Z));
            let front = view.rotation * Vec3::NEG_Z;
            (Quat::from_mat3(&Mat3::from_cols(x, y, x.cross(y))), non_square_scale(front.to_array().into(), x.to_array().into(), y.to_array().into()))
        } else {
            (view.rotation, 1.0)
        };
        let size = CORONA_SIZE * corona.draw_scale * factor;
        let faced = Transform { translation, rotation: facing, scale: Vec3::new(size * corona.draw_scale3d[0], size * corona.draw_scale3d[1], 1.0) };
        let color = corona_color(corona.hue, corona.saturation, bright).map(|c| c * scale);
        if *transform != faced {
            *transform = faced;
        }
        if quad.color != color
            && let Some(mut mesh) = meshes.get_mut(&mesh.0)
        {
            let [r, g, b] = color;
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[r, g, b, 1.0]; 4]);
            quad.color = color;
        }
        shown.set_if_neq(Visibility::Inherited);
    }
}
