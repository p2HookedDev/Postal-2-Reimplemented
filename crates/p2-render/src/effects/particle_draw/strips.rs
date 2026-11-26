use bevy::math::Vec3;
use bevy::mesh::{Indices, Mesh};
use p2_bevy_assets::to_bevy;
use p2_math::Vector;
use p2_particles::{Beam, StripPoint};

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

pub(crate) fn empty_quads(mesh: &mut Mesh) {
    Quads::default().write(mesh);
}

/// Retail draws one quad fewer than it builds (a stale index), so the oldest pair of points is not drawn.
pub(crate) fn fill_strip(mesh: &mut Mesh, points: &[StripPoint], normal: Option<Vec3>, eye: Vec3) {
    let mut quads = Quads::default();
    if points.len() > 2 {
        strip_quads(&mut quads, &points[..points.len() - 1], normal, eye, |_| [1.0, 0.0]);
    }
    quads.write(mesh);
}

pub(crate) fn fill_smooth_strip(mesh: &mut Mesh, points: &[StripPoint], normal: Option<Vec3>, eye: Vec3) {
    let mut quads = Quads::default();
    if points.len() > 2 {
        let smooth = ribbon(&points[..points.len() - 1]);
        let pieces = PIECES as f32;
        strip_quads(&mut quads, &smooth, normal, eye, |i| {
            let k = (i % PIECES) as f32;
            [1.0 - k / pieces, 1.0 - (k + 1.0) / pieces]
        });
    }
    quads.write(mesh);
}

const PIECES: usize = 4;

fn ribbon(points: &[StripPoint]) -> Vec<StripPoint> {
    let last = points.len().saturating_sub(1);
    let at = |i: usize| Vec3::from_array(points[i.min(last)].location.into());
    let mut out = Vec::with_capacity(last * PIECES + 1);
    for i in 0..last {
        let (p0, p1, p2, p3) = (at(i.saturating_sub(1)), at(i), at(i + 1), at(i + 2));
        let (from, to) = (points[i], points[i + 1]);
        for k in 0..PIECES {
            let t = k as f32 / PIECES as f32;
            let curve = 0.5 * (2.0 * p1 + (p2 - p0) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t);
            out.push(StripPoint { location: Vector::from(curve.to_array()), size: from.size + (to.size - from.size) * t, color: from.color });
        }
    }
    out.extend(points.get(last).copied());
    out
}

fn strip_quads(quads: &mut Quads, points: &[StripPoint], normal: Option<Vec3>, eye: Vec3, v: impl Fn(usize) -> [f32; 2]) {
    let at = |i: usize| Vec3::from_array(points[i].location.into());
    let sides: Vec<Vec3> = (0..points.len())
        .map(|i| {
            let previous = if i == 0 { 2.0 * at(0) - at(1) } else { at(i - 1) };
            let facing = normal.unwrap_or_else(|| (eye - previous).normalize_or_zero());
            (at(i) - previous).cross(facing).normalize_or_zero() * points[i].size
        })
        .collect();
    for i in 0..points.len() - 1 {
        let color = display(points[i].color);
        let [v0, v1] = v(i);
        let corners = [at(i) - sides[i], at(i) + sides[i], at(i + 1) + sides[i + 1], at(i + 1) - sides[i + 1]];
        quads.push(corners, [0.0, v0, 1.0, v1], [color; 4]);
    }
}

pub(crate) fn fill_beams(mesh: &mut Mesh, beams: &[Beam], [u_scale, v_scale]: [f32; 2], sheets: usize, eye: Vec3) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{PrimitiveTopology, VertexAttributeValues};

    fn point(x: f32, z: f32) -> StripPoint {
        StripPoint { location: [x, 0.0, z].into(), size: 2.0, color: [255; 4] }
    }

    fn at(p: &StripPoint) -> Vec3 {
        Vec3::from_array(p.location.into())
    }

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

    #[test]
    fn ribbon_passes_through_the_script_points() {
        let points = [point(0.0, 0.0), point(10.0, -5.0), point(20.0, -20.0), point(25.0, -40.0)];
        let smooth = ribbon(&points);
        assert_eq!(smooth.len(), 3 * PIECES + 1);
        for (i, p) in points.iter().enumerate() {
            assert!(at(&smooth[i * PIECES]).abs_diff_eq(at(p), 1e-4), "{i}");
        }
    }

    #[test]
    fn a_straight_stream_stays_straight() {
        assert!(ribbon(&[point(0.0, 0.0), point(10.0, 0.0), point(20.0, 0.0)]).iter().all(|p| at(p).z == 0.0));
        assert!(ribbon(&[point(0.0, 0.0), point(0.0, -10.0), point(0.0, -20.0)]).iter().all(|p| at(p).x.abs() < 1e-4));
    }
}
