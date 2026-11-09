use super::*;
use p2_math::Vector;
use p2_particles::{Beam, FluidOscillator, FluidSettings, FluidSurface, FluidVertex, StripPoint, View};
use p2_assets::{UvModifier, uv_matrix};
use crate::mesh_view::{MaterialImages, SceneLights, SectionMaterial, SectionView, actor_standard, section_material, set_uv_transform};

pub(super) fn camera_view(camera: &Transform, origin: Vec3) -> View {
    let unreal = |v: Vec3| Vector::from(to_bevy(v.to_array()));
    View { location: unreal(camera.translation - origin), right: unreal(camera.rotation * Vec3::X), up: unreal(camera.rotation * Vec3::Y) }
}

#[derive(Default)]
struct Quads {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Quads {
    fn push(&mut self, corners: [Vec3; 4], [u0, v0, u1, v1]: [f32; 4], colors: [[f32; 4]; 4]) {
        let base = self.positions.len() as u32;
        self.positions.extend(corners.map(|c| to_bevy(c.to_array())));
        self.uvs.extend([[u0, v0], [u1, v0], [u1, v1], [u0, v1]]);
        self.colors.extend(colors);
        self.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn write(mut self, mesh: &mut Mesh) {
        // No quads: one invisible quad of no size (the renderer warns about meshes without vertices).
        if self.positions.is_empty() {
            self.push([Vec3::ZERO; 4], [0.0; 4], [[0.0; 4]; 4]);
        }
        let vertices = self.positions.len();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; vertices]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, self.uvs.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(Indices::U32(self.indices));
    }
}

fn display(color: [u8; 4]) -> [f32; 4] {
    color.map(|c| f32::from(c) / 255.0)
}

pub(super) fn empty_quads(mesh: &mut Mesh) {
    Quads::default().write(mesh);
}

/// Side of a point: direction from the previous point × `normal`, else × the direction to the eye.
/// Retail draws one quad fewer than it builds (a stale index): the oldest pair of points is not drawn.
pub(super) fn fill_strip(mesh: &mut Mesh, points: &[StripPoint], normal: Option<Vec3>, eye: Vec3) {
    let mut quads = Quads::default();
    if points.len() > 2 {
        let at = |i: usize| Vec3::from_array(points[i].location.into());
        let sides: Vec<Vec3> = (0..points.len())
            .map(|i| {
                let previous = if i == 0 { 2.0 * at(0) - at(1) } else { at(i - 1) };
                let facing = normal.unwrap_or_else(|| (eye - previous).normalize_or_zero());
                (at(i) - previous).cross(facing).normalize_or_zero() * points[i].size
            })
            .collect();
        for i in 0..points.len() - 2 {
            let color = display(points[i].color);
            let corners = [at(i) - sides[i], at(i) + sides[i], at(i + 1) + sides[i + 1], at(i + 1) - sides[i + 1]];
            quads.push(corners, [0.0, 1.0, 1.0, 0.0], [color; 4]);
        }
    }
    quads.write(mesh);
}

/// Side of a point: direction from the previous point × the line from the eye to that previous point,
/// turned about the direction by 0x8000 / `sheets` per sheet after the first.
pub(super) fn fill_beams(mesh: &mut Mesh, beams: &[Beam], [u_scale, v_scale]: [f32; 2], sheets: usize, eye: Vec3) {
    let mut quads = Quads::default();
    for beam in beams.iter().filter(|beam| beam.points.len() > 1) {
        let color = display(beam.color);
        let at = |i: usize| Vec3::from_array(beam.points[i].location.into());
        for sheet in 0..sheets {
            let turn = p2_math::radians((0x8000 / sheets * sheet) as i32);
            let base = quads.positions.len() as u32;
            let mut previous = 2.0 * at(0) - at(1);
            for (i, point) in beam.points.iter().enumerate() {
                let here = at(i);
                let direction = here - previous;
                let mut side = direction.cross(previous - eye).normalize_or_zero();
                if sheet != 0 {
                    side = bevy::math::Quat::from_axis_angle(direction.normalize_or_zero(), turn) * side;
                }
                let side = side * beam.size;
                let u = u_scale * point.fraction;
                quads.positions.extend([to_bevy((here + side).to_array()), to_bevy((here - side).to_array())]);
                quads.uvs.extend([[u, 0.0], [u, v_scale]]);
                quads.colors.extend([color; 2]);
                previous = here;
            }
            for i in 0..beam.points.len() as u32 - 1 {
                let first = base + i * 2;
                quads.indices.extend([first, first + 1, first + 2, first + 1, first + 3, first + 2]);
            }
        }
    }
    quads.write(mesh);
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
    use p2_vm::Value;
    if !vm.store.is_a(actor, "FluidSurfaceInfo") {
        return None;
    }
    let float = |object: ObjectId, name: &str, default: f32| match vm.store.get(object, name) {
        Some(Value::Float(value)) => value,
        _ => default,
    };
    let byte = |object: ObjectId, name: &str, default: u8| match vm.store.get(object, name) {
        Some(Value::Byte(value)) => value,
        _ => default,
    };
    let int = |name: &str, default: i32| match vm.store.get(actor, name) {
        Some(Value::Int(value)) => value,
        _ => default,
    };
    let d = FluidSettings::default();
    let noise_strength = match vm.store.get(actor, "FluidNoiseStrength") {
        Some(Value::Struct(range)) => match range.as_slice() {
            [Value::Float(min), Value::Float(max), ..] => (*min, *max),
            _ => d.noise_strength,
        },
        _ => d.noise_strength,
    };
    let settings = FluidSettings {
        hexagonal: byte(actor, "FluidGridType", u8::from(d.hexagonal)) == 1,
        spacing: float(actor, "FluidGridSpacing", d.spacing),
        x_size: int("FluidXSize", d.x_size),
        y_size: int("FluidYSize", d.y_size),
        height_scale: float(actor, "FluidHeightScale", d.height_scale),
        speed: float(actor, "FluidSpeed", d.speed),
        damping: float(actor, "FluidDamping", d.damping),
        noise_frequency: float(actor, "FluidNoiseFrequency", d.noise_frequency),
        noise_strength,
        u_tiles: float(actor, "UTiles", d.u_tiles),
        u_offset: float(actor, "UOffset", d.u_offset),
        v_tiles: float(actor, "VTiles", d.v_tiles),
        v_offset: float(actor, "VOffset", d.v_offset),
        alpha_curve_scale: float(actor, "AlphaCurveScale", d.alpha_curve_scale),
        alpha_height_scale: float(actor, "AlphaHeightScale", d.alpha_height_scale),
        alpha_max: byte(actor, "AlphaMax", d.alpha_max),
        ripple_velocity_factor: float(actor, "RippleVelocityFactor", d.ripple_velocity_factor),
        warm_up_time: float(actor, "WarmUpTime", d.warm_up_time),
        update_rate: float(actor, "UpdateRate", d.update_rate),
    };
    let clamp_bitmap = match vm.store.get(actor, "ClampBitmap") {
        Some(Value::Array(words)) => words.iter().map(|word| if let Value::Int(word) = word { *word } else { 0 }).collect(),
        _ => Vec::new(),
    };
    let skin = match vm.store.get(actor, "Skins") {
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
        .filter(|&o| !vm.store.is_deleted(o) && vm.store.is_a(o, "FluidSurfaceOscillator") && vm.store.get(o, "FluidInfo") == Some(Value::Object(Some(actor))))
        .map(|o| FluidOscillator {
            location: vm.store.location(o),
            frequency: float(o, "Frequency", 1.0),
            phase: byte(o, "Phase", 0),
            strength: float(o, "Strength", 10.0),
            radius: float(o, "Radius", 0.0),
            time: 0.0,
        })
        .collect();
    let s = &surface.settings;
    let half = Vector::new((s.x_size - 1).max(0) as f32 * 0.5 * s.spacing, (s.y_size - 1).max(0) as f32 * 0.5 * s.spacing * s.row_spacing(), s.spacing);
    Some(FluidActor { object: actor, bounds: (location - half, location + half), skin, hidden: vm.store.flag_of(actor, "bHidden"), surface })
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
            let material = section_material(&section, actor_standard(&section, SceneLights::Ignored), &mut t.assets.materials, &mut t.assets.mesh_lit);
            let shown = if fluid.hidden { Visibility::Hidden } else { Visibility::Inherited };
            let parent = t.commands.spawn((ActorModel(fluid.object), Transform::from_translation(origin), Visibility::default())).id();
            let mut spawned = t.commands.spawn((Mesh3d(mesh.clone()), Transform::default(), shown, NoFrustumCulling, ChildOf(parent)));
            material.insert(&mut spawned);
            let entity = spawned.id();
            self.fluids.push(FluidShown {
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

    /// Before `update_particles` takes the presented frame's step. A surface steps only while in view
    /// within the last second (`drawn_lately`); only surfaces in view get their vertices rebuilt.
    pub(super) fn update_fluids(&mut self, t: &mut SceneTargets) {
        let Some(world) = t.play.world_mut() else { return };
        let plings = world.take_plings();
        if self.fluids.is_empty() {
            return;
        }
        let step = world.step();
        if t.particle_view.cameras.iter().next().is_none() {
            return;
        }
        let in_frustum = t.particle_view.frustum_test(world.origin());
        for fluid in &mut self.fluids {
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
            if drawn && let Some(mut mesh) = t.assets.meshes.get_mut(&fluid.mesh) {
                refill_fluid(&mut mesh, &mut fluid.surface, &mut fluid.vertices);
            }
            fluid.seconds += step;
            if drawn && !fluid.uv.is_empty() {
                let [[a, b, c], [d, e, f]] = uv_matrix(&fluid.uv, fluid.seconds, fluid.uv_size);
                let transform = bevy::math::Affine2::from_cols(Vec2::new(a, d), Vec2::new(b, e), Vec2::new(c, f));
                match &fluid.material {
                    SectionMaterial::MeshLit(handle) => set_uv_transform(&mut t.assets.mesh_lit, handle, transform, |m| &m.base, |m| &mut m.base),
                    SectionMaterial::Standard(handle) => set_uv_transform(&mut t.assets.materials, handle, transform, |m| m, |m| m),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    fn positions(mesh: &Mesh) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { panic!("no positions") };
        positions.iter().map(|&p| Vec3::from(to_bevy(p))).collect()
    }

    fn eye() -> Vec3 {
        Vec3::new(-1000.0, 0.0, 0.0)
    }

    #[test]
    fn strip_is_a_ribbon_facing_the_eye() {
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let falling = [0.0, -10.0, -20.0, -30.0].map(|z| StripPoint { location: [0.0, 0.0, z].into(), size: 2.0, color: [255; 4] });
        fill_strip(&mut mesh, &falling, None, eye());
        let p = positions(&mesh);
        assert_eq!(p.len(), 8);
        for corner in &p {
            assert!((corner.y.abs() - 2.0).abs() < 1e-4 && corner.x.abs() < 1e-4, "{p:?}");
        }
        assert_eq!((p[0].z, p[2].z), (0.0, -10.0));
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0) else { panic!() };
        assert_eq!((uvs[0][1], uvs[2][1]), (1.0, 0.0));
        fill_strip(&mut mesh, &falling[..2], None, eye());
        assert!(positions(&mesh).iter().all(|&c| c == Vec3::ZERO));
    }

    #[test]
    fn beam_sheets_turn_about_the_beam() {
        use p2_particles::BeamPoint;
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let points = [(0.0, 0.0), (-10.0, 0.5), (-20.0, 1.0)].map(|(z, fraction)| BeamPoint { location: [0.0, 0.0, z].into(), fraction }).to_vec();
        fill_beams(&mut mesh, &[Beam { points, size: 2.0, color: [255; 4] }], [2.0, 3.0], 2, eye());
        let p = positions(&mesh);
        assert_eq!(p.len(), 12);
        for corner in &p[..6] {
            assert!((corner.y.abs() - 2.0).abs() < 1e-4 && corner.x.abs() < 1e-4, "{p:?}");
        }
        for corner in &p[6..] {
            assert!((corner.x.abs() - 2.0).abs() < 1e-4 && corner.y.abs() < 1e-4, "{p:?}");
        }
        assert_eq!(p[0] + p[1], Vec3::ZERO);
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0) else { panic!() };
        assert_eq!((uvs[0], uvs[1], uvs[2], uvs[5]), ([0.0, 0.0], [0.0, 3.0], [1.0, 0.0], [2.0, 3.0]));
        let indices: Vec<usize> = mesh.indices().expect("indices").iter().collect();
        assert_eq!(indices.len(), 24);
        assert_eq!(indices[..6], [0, 1, 2, 1, 3, 2]);
        assert_eq!(indices[12..18], [6, 7, 8, 7, 9, 8]);
    }
}
