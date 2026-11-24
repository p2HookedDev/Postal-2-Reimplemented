use super::*;
use p2_math::Vector;
use p2_particles::{FluidOscillator, FluidSettings, FluidSurface, View};
use p2_render::fluids::FluidDrawn;
use p2_render::scene::MaterialImages;

pub(super) fn camera_view(camera: &Transform, origin: Vec3) -> View {
    let unreal = |v: Vec3| Vector::from(to_bevy(v.to_array()));
    View { location: unreal(camera.translation - origin), right: unreal(camera.rotation * Vec3::X), up: unreal(camera.rotation * Vec3::Y) }
}

pub(super) struct FluidActor {
    object: ObjectId,
    surface: FluidSurface,
    skin: Option<String>,
    bounds: (Vector, Vector),
    hidden: bool,
}

pub(super) fn fluid_actor(vm: &Vm<Arc<Loader>>, actor: ObjectId, seed: u32) -> Option<FluidActor> {
    use p2_vm::{Prop, Value};
    if !vm.store.is_a(actor, "FluidSurfaceInfo") {
        return None;
    }
    let float = |object: ObjectId, property: Prop, default: f32| match vm.store.get(object, property) {
        Some(Value::Float(value)) => value,
        _ => default,
    };
    let byte = |object: ObjectId, property: Prop, default: u8| match vm.store.get(object, property) {
        Some(Value::Byte(value)) => value,
        _ => default,
    };
    let int = |property: Prop, default: i32| match vm.store.get(actor, property) {
        Some(Value::Int(value)) => value,
        _ => default,
    };
    let d = FluidSettings::default();
    let noise_strength = match vm.store.get(actor, Prop::FluidNoiseStrength) {
        Some(Value::Struct(range)) => match range.as_slice() {
            [Value::Float(min), Value::Float(max), ..] => (*min, *max),
            _ => d.noise_strength,
        },
        _ => d.noise_strength,
    };
    let settings = FluidSettings {
        hexagonal: byte(actor, Prop::FluidGridType, u8::from(d.hexagonal)) == 1,
        spacing: float(actor, Prop::FluidGridSpacing, d.spacing),
        x_size: int(Prop::FluidXSize, d.x_size),
        y_size: int(Prop::FluidYSize, d.y_size),
        height_scale: float(actor, Prop::FluidHeightScale, d.height_scale),
        speed: float(actor, Prop::FluidSpeed, d.speed),
        damping: float(actor, Prop::FluidDamping, d.damping),
        noise_frequency: float(actor, Prop::FluidNoiseFrequency, d.noise_frequency),
        noise_strength,
        u_tiles: float(actor, Prop::UTiles, d.u_tiles),
        u_offset: float(actor, Prop::UOffset, d.u_offset),
        v_tiles: float(actor, Prop::VTiles, d.v_tiles),
        v_offset: float(actor, Prop::VOffset, d.v_offset),
        alpha_curve_scale: float(actor, Prop::AlphaCurveScale, d.alpha_curve_scale),
        alpha_height_scale: float(actor, Prop::AlphaHeightScale, d.alpha_height_scale),
        alpha_max: byte(actor, Prop::AlphaMax, d.alpha_max),
        ripple_velocity_factor: float(actor, Prop::RippleVelocityFactor, d.ripple_velocity_factor),
        warm_up_time: float(actor, Prop::WarmUpTime, d.warm_up_time),
        update_rate: float(actor, Prop::UpdateRate, d.update_rate),
    };
    let clamp_bitmap = match vm.store.get(actor, Prop::ClampBitmap) {
        Some(Value::Array(words)) => words.iter().map(|word| if let Value::Int(word) = word { *word } else { 0 }).collect(),
        _ => Vec::new(),
    };
    let skin = match vm.store.get(actor, Prop::Skins) {
        Some(Value::Array(skins)) => match skins.first() {
            Some(Value::Object(Some(material))) => Some(vm.store.object(*material).path.clone()),
            _ => None,
        },
        _ => None,
    };
    let location = vm.store.location(actor);
    let mut surface = FluidSurface::new(settings, location, clamp_bitmap, seed);
    surface.oscillators = vm
        .actors()
        .filter(|&o| !vm.store.is_deleted(o) && vm.store.is_a(o, "FluidSurfaceOscillator") && vm.store.get(o, Prop::FluidInfo) == Some(Value::Object(Some(actor))))
        .map(|o| FluidOscillator {
            location: vm.store.location(o),
            frequency: float(o, Prop::Frequency, 1.0),
            phase: byte(o, Prop::Phase, 0),
            strength: float(o, Prop::Strength, 10.0),
            radius: float(o, Prop::Radius, 0.0),
            time: 0.0,
        })
        .collect();
    let s = &surface.settings;
    let half = Vector::new((s.x_size - 1).max(0) as f32 * 0.5 * s.spacing, (s.y_size - 1).max(0) as f32 * 0.5 * s.spacing * s.row_spacing(), s.spacing);
    Some(FluidActor { object: actor, bounds: (location - half, location + half), skin, hidden: vm.store.flag_of(actor, Prop::bHidden), surface })
}

pub(super) struct FluidShown {
    object: ObjectId,
    surface: FluidSurface,
    entity: Entity,
    bounds: (Vector, Vector),
    shown: bool,
    since_drawn: f32,
    seconds: f32,
}

#[derive(Resource, Default)]
pub struct Fluids(Vec<FluidShown>);

impl Session {
    pub(super) fn add_fluids(&mut self, fluids: Vec<FluidActor>, origin: Vec3, t: &mut SceneTargets) {
        if fluids.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for mut fluid in fluids {
            let Some((package, export)) = fluid.skin.as_deref().and_then(|skin| game.export_by_path(skin)) else { continue };
            let Some(texture) = game.fluid_texture(package, export_reference(export)) else {
                bevy::log::warn!("fluid surface not drawn, no material from skin {:?}", fluid.skin);
                continue;
            };
            let images = MaterialImages {
                image: t.assets.images.add(texture.image),
                glow: texture.glow.map(|glow| t.assets.images.add(glow)),
                detail: texture.detail.map(|detail| t.assets.images.add(detail)),
                specular: texture.specular.map(|(cube, params)| (t.assets.images.add(cube), params)),
                state: texture.state,
            };
            let shown = if fluid.hidden { Visibility::Hidden } else { Visibility::Inherited };
            let parent = t.commands.spawn((MapScoped, ActorModel(fluid.object), Transform::from_translation(origin), Visibility::default())).id();
            let drawn = FluidDrawn { images, vertices: fluid.surface.vertices(), indices: fluid.surface.indices(), seconds: 0.0 };
            let entity = t.commands.spawn((drawn, Transform::default(), shown, ChildOf(parent))).id();
            t.fluids.0.push(FluidShown { object: fluid.object, surface: fluid.surface, entity, bounds: fluid.bounds, shown: !fluid.hidden, since_drawn: 0.0, seconds: 0.0 });
        }
    }
}

#[derive(SystemParam)]
pub(super) struct FluidParams<'w, 's> {
    fluids: ResMut<'w, Fluids>,
    play: ResMut<'w, Play>,
    cameras: Query<'w, 's, &'static Transform, With<SceneView>>,
    projections: Query<'w, 's, &'static Projection, With<SceneView>>,
    commands: Commands<'w, 's>,
    drawn: Query<'w, 's, &'static mut FluidDrawn>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_fluids(mut t: FluidParams) {
    let started = std::time::Instant::now();
    update_fluids(&mut t);
    t.times.add("fluids", started);
}

fn update_fluids(t: &mut FluidParams) {
    let Some(world) = t.play.world_mut() else { return };
    let plings = world.take_plings();
    if t.fluids.0.is_empty() {
        return;
    }
    let step = if world.game_paused() { 0.0 } else { world.step() };
    if t.cameras.iter().next().is_none() {
        return;
    }
    let in_frustum = frustum_test(&t.cameras, &t.projections, world.origin());
    for fluid in &mut t.fluids.0 {
        for pling in plings.iter().filter(|pling| pling.surface == fluid.object) {
            fluid.surface.pling(pling.position, pling.strength, pling.radius);
        }
        if let Some(actor) = world.actor(fluid.object) {
            let shown = !(actor.gone || actor.hidden);
            if shown != fluid.shown {
                fluid.shown = shown;
                t.commands.entity(fluid.entity).try_insert(if shown { Visibility::Inherited } else { Visibility::Hidden });
            }
        }
        let (min, max) = (Vec3::from(to_bevy(fluid.bounds.0)), Vec3::from(to_bevy(fluid.bounds.1)));
        let in_view = in_frustum(Some((min.min(max), min.max(max))));
        let drawn = fluid.shown && in_view;
        fluid.since_drawn = if drawn { 0.0 } else { fluid.since_drawn + step };
        if step <= 0.0 {
            continue;
        }
        let touchers = world.fluid_touchers(fluid.object);
        fluid.surface.tick(step, fluid.since_drawn <= 1.0, touchers);
        fluid.seconds += step;
        if drawn && let Ok(mut handed) = t.drawn.get_mut(fluid.entity) {
            fluid.surface.vertices_into(&mut handed.vertices);
            handed.seconds = fluid.seconds;
        }
    }
}
