use super::*;
use p2_math::Vector;
use p2_particles::{FluidOscillator, FluidSettings, FluidSurface, FluidVertex, View};
use p2_assets::{UvModifier, uv_matrix};
use p2_render::view::SectionMaterial;
use p2_render::scene::{MaterialImages, SectionView};

pub(super) fn camera_view(camera: &Transform, origin: Vec3) -> View {
    let unreal = |v: Vec3| Vector::from(to_bevy(v.to_array()));
    View { location: unreal(camera.translation - origin), right: unreal(camera.rotation * Vec3::X), up: unreal(camera.rotation * Vec3::Y) }
}

/// The vertex colour is the light, 1 being neutral. No water skin reads the simulation's height alpha; retail's use of it is
/// (unverified).
pub(super) fn fill_fluid(mesh: &mut Mesh, surface: &mut FluidSurface) {
    let vertices = surface.vertices();
    let mut indices = surface.indices();
    let (positions, normals, colors, uvs): (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<[f32; 2]>) = if vertices.is_empty() || indices.is_empty() {
        indices = vec![0, 0, 0];
        (vec![[0.0; 3]], vec![[0.0, 1.0, 0.0]], vec![[0.0; 4]], vec![[0.0; 2]])
    } else {
        (
            vertices.iter().map(|v| to_bevy(v.position)).collect(),
            vertices.iter().map(|v| Vec3::from(to_bevy(v.normal)).normalize_or_zero().to_array()).collect(),
            vec![[1.0; 4]; vertices.len()],
            vertices.iter().map(|v| v.uv).collect(),
        )
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

fn refill_fluid(mesh: &mut Mesh, surface: &mut FluidSurface, vertices: &mut Vec<FluidVertex>) {
    use bevy::mesh::VertexAttributeValues::Float32x3;
    surface.vertices_into(vertices);
    if vertices.is_empty() || mesh.count_vertices() != vertices.len() {
        return;
    }
    if let Some(Float32x3(positions)) = mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION) {
        for (position, v) in positions.iter_mut().zip(vertices.iter()) {
            *position = to_bevy(v.position);
        }
    }
    if let Some(Float32x3(normals)) = mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL) {
        for (normal, v) in normals.iter_mut().zip(vertices.iter()) {
            *normal = Vec3::from(to_bevy(v.normal)).normalize_or_zero().to_array();
        }
    }
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
    vertices: Vec<FluidVertex>,
    mesh: Handle<Mesh>,
    entity: Entity,
    bounds: (Vector, Vector),
    shown: bool,
    since_drawn: f32,
    material: SectionMaterial,
    uv: Vec<UvModifier>,
    uv_size: [f32; 2],
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
            let uv_size = texture.image.size().as_vec2().to_array();
            let found = MaterialImages {
                image: t.assets.images.add(texture.image),
                glow: texture.glow.map(|glow| t.assets.images.add(glow)),
                detail: texture.detail.map(|detail| t.assets.images.add(detail)),
                specular: texture.specular.map(|(cube, params)| (t.assets.images.add(cube), params)),
                state: texture.state,
            };
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            fill_fluid(&mut mesh, &mut fluid.surface);
            let mesh = t.assets.meshes.add(mesh);
            let mut section = SectionView::textured(mesh.clone(), Some(found));
            section.no_scene_lights = true;
            section.two_sided = true;
            let material = t.assets.actor_materials.material(&mut ActorLook::Fluid, &section);
            let shown = if fluid.hidden { Visibility::Hidden } else { Visibility::Inherited };
            let parent = t.commands.spawn((MapScoped, ActorModel(fluid.object), Transform::from_translation(origin), Visibility::default())).id();
            let mut spawned = t.commands.spawn((Mesh3d(mesh.clone()), Transform::default(), shown, NoFrustumCulling, ChildOf(parent)));
            material.insert(&mut spawned);
            let entity = spawned.id();
            t.fluids.0.push(FluidShown {
                object: fluid.object,
                surface: fluid.surface,
                vertices: Vec::new(),
                mesh,
                entity,
                bounds: fluid.bounds,
                shown: !fluid.hidden,
                since_drawn: 0.0,
                material,
                uv: section.uv,
                uv_size,
                seconds: 0.0,
            });
        }
    }
}

pub(super) struct FluidTargets<'a, 'w, 's> {
    pub(super) fluids: &'a mut Fluids,
    pub(super) play: &'a mut Play,
    pub(super) cameras: &'a Query<'w, 's, &'static Transform, With<SceneView>>,
    pub(super) projections: &'a Query<'w, 's, &'static Projection, With<SceneView>>,
    pub(super) commands: &'a mut Commands<'w, 's>,
    pub(super) meshes: &'a mut Assets<Mesh>,
    pub(super) materials: &'a mut ActorMaterials<'w>,
}

#[derive(SystemParam)]
pub(super) struct FluidParams<'w, 's> {
    fluids: ResMut<'w, Fluids>,
    play: ResMut<'w, Play>,
    cameras: Query<'w, 's, &'static Transform, With<SceneView>>,
    projections: Query<'w, 's, &'static Projection, With<SceneView>>,
    commands: Commands<'w, 's>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ActorMaterials<'w>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_fluids(mut t: FluidParams) {
    let started = std::time::Instant::now();
    update_fluids(&mut FluidTargets {
        fluids: &mut t.fluids,
        play: &mut t.play,
        cameras: &t.cameras,
        projections: &t.projections,
        commands: &mut t.commands,
        meshes: &mut t.meshes,
        materials: &mut t.materials,
    });
    t.times.add("fluids", started);
}

/// A surface steps only while in view within the last second (`drawn_lately`); only surfaces in view get their
/// vertices rebuilt.
pub(super) fn update_fluids(t: &mut FluidTargets) {
    let Some(world) = t.play.world_mut() else { return };
    let plings = world.take_plings();
    if t.fluids.0.is_empty() {
        return;
    }
    let step = if world.game_paused() { 0.0 } else { world.step() };
    if t.cameras.iter().next().is_none() {
        return;
    }
    let in_frustum = frustum_test(t.cameras, t.projections, world.origin());
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
        if drawn && let Some(mut mesh) = t.meshes.get_mut(&fluid.mesh) {
            p2_host::frame_times::mesh_written(p2_host::frame_times::MeshWrite::Fluid);
            refill_fluid(&mut mesh, &mut fluid.surface, &mut fluid.vertices);
        }
        fluid.seconds += step;
        if drawn && !fluid.uv.is_empty() {
            let [[a, b, c], [d, e, f]] = uv_matrix(&fluid.uv, fluid.seconds, fluid.uv_size);
            let transform = bevy::math::Affine2::from_cols(Vec2::new(a, d), Vec2::new(b, e), Vec2::new(c, f));
            t.materials.set_uv(&fluid.material, transform);
        }
    }
}
