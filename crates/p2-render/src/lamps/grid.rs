use bevy::math::Vec3;
use p2_assets::{ModelPolygon, poly_flags};
use p2_bevy_assets::to_bevy;
use rustc_hash::FxHashMap;

/// World units per texel of the lamp visibility grid.
pub const LAMP_TEXEL: f32 = 16.0;
pub const PATCH_TEXELS: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sides {
    One,
    Two,
}

/// One coplanar BSP surface's chart, in Bevy space: grid texel (u, v) sits at `origin + (u * U + v * V) * LAMP_TEXEL`.
#[derive(Clone, Debug)]
pub struct ShadowSurface {
    pub origin: [f32; 3],
    pub u: [f32; 3],
    pub v: [f32; 3],
    pub normal: [f32; 3],
    pub size: [u32; 2],
    pub sides: Sides,
}

impl ShadowSurface {
    pub fn texel(&self, point: [f32; 3]) -> [f32; 2] {
        let offset = Vec3::from(point) - Vec3::from(self.origin);
        [offset.dot(Vec3::from(self.u)) / LAMP_TEXEL, offset.dot(Vec3::from(self.v)) / LAMP_TEXEL]
    }
}

struct Chart {
    normal: Vec3,
    u: Vec3,
    v: Vec3,
    depth: f32,
    low: [f32; 2],
    high: [f32; 2],
    sides: Sides,
}

/// One chart per `ModelPolygon.surface`; unlit polygons get none, and the caller leaves sky polygons out.
/// Returns the charts and each polygon's chart id.
pub fn chart_polygons(polygons: &[&ModelPolygon]) -> (Vec<ShadowSurface>, Vec<Option<u32>>) {
    let mut charts: Vec<Chart> = Vec::new();
    let mut by_surface: FxHashMap<usize, u32> = FxHashMap::default();
    let ids = polygons
        .iter()
        .map(|polygon| {
            if polygon.poly_flags & poly_flags::UNLIT != 0 {
                return None;
            }
            let id = match by_surface.get(&polygon.surface) {
                Some(&id) => id,
                None => {
                    let normal = Vec3::from(to_bevy(polygon.normal)).try_normalize()?;
                    let (u, v) = normal.any_orthonormal_pair();
                    let depth = Vec3::from(to_bevy(*polygon.positions.first()?)).dot(normal);
                    charts.push(Chart { normal, u, v, depth, low: [f32::MAX; 2], high: [f32::MIN; 2], sides: Sides::One });
                    let id = (charts.len() - 1) as u32;
                    by_surface.insert(polygon.surface, id);
                    id
                }
            };
            let chart = &mut charts[id as usize];
            if polygon.poly_flags & poly_flags::TWO_SIDED != 0 {
                chart.sides = Sides::Two;
            }
            for &position in &polygon.positions {
                let point = Vec3::from(to_bevy(position));
                for (axis, along) in [chart.u, chart.v].into_iter().enumerate() {
                    chart.low[axis] = chart.low[axis].min(point.dot(along));
                    chart.high[axis] = chart.high[axis].max(point.dot(along));
                }
            }
            Some(id)
        })
        .collect();
    let surfaces = charts
        .into_iter()
        .map(|chart| {
            let extent = [0, 1].map(|axis| chart.high[axis] - chart.low[axis]);
            ShadowSurface {
                origin: (chart.normal * chart.depth + chart.u * chart.low[0] + chart.v * chart.low[1]).to_array(),
                u: chart.u.to_array(),
                v: chart.v.to_array(),
                normal: chart.normal.to_array(),
                size: extent.map(|extent| (extent / LAMP_TEXEL).ceil() as u32 + 1),
                sides: chart.sides,
            }
        })
        .collect();
    (surfaces, ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polygon(surface: usize, poly_flags: u32, positions: Vec<[f32; 3]>) -> ModelPolygon {
        ModelPolygon { node: 0, surface, material: 0, poly_flags, normal: [0.0, 0.0, 1.0], positions, uvs: Vec::new() }
    }

    fn inside(surface: &ShadowSurface, polygons: &[ModelPolygon]) -> bool {
        polygons.iter().flat_map(|p| &p.positions).all(|&p| {
            let [u, v] = surface.texel(to_bevy(p));
            (-1e-3..surface.size[0] as f32).contains(&u) && (-1e-3..surface.size[1] as f32).contains(&v)
        })
    }

    #[test]
    fn quad_gets_one_chart() {
        let quad = [polygon(3, 0, vec![[0.0, 0.0, 8.0], [64.0, 0.0, 8.0], [64.0, 40.0, 8.0], [0.0, 40.0, 8.0]])];
        let (surfaces, ids) = chart_polygons(&quad.iter().collect::<Vec<_>>());
        assert_eq!((surfaces.len(), ids), (1, vec![Some(0)]));
        let mut size = surfaces[0].size;
        size.sort_unstable();
        assert_eq!(size, [4, 5]);
        assert_eq!(surfaces[0].sides, Sides::One);
        assert!(inside(&surfaces[0], &quad));
    }

    #[test]
    fn l_shape_on_one_plane_shares_a_chart() {
        let shape = [
            polygon(1, 0, vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [100.0, 20.0, 0.0], [0.0, 20.0, 0.0]]),
            polygon(1, 0, vec![[0.0, 20.0, 0.0], [20.0, 20.0, 0.0], [20.0, 100.0, 0.0], [0.0, 100.0, 0.0]]),
        ];
        let (surfaces, ids) = chart_polygons(&shape.iter().collect::<Vec<_>>());
        assert_eq!((surfaces.len(), ids), (1, vec![Some(0), Some(0)]));
        assert_eq!(surfaces[0].size, [8, 8]);
        assert!(inside(&surfaces[0], &shape));
    }

    #[test]
    fn two_sided_and_unlit() {
        let polygons = [
            polygon(0, poly_flags::TWO_SIDED, vec![[0.0; 3], [32.0, 0.0, 0.0], [0.0, 32.0, 0.0]]),
            polygon(2, poly_flags::UNLIT, vec![[0.0; 3], [32.0, 0.0, 0.0], [0.0, 32.0, 0.0]]),
            polygon(5, 0, vec![[0.0, 0.0, 50.0], [32.0, 0.0, 50.0], [0.0, 32.0, 50.0]]),
        ];
        let (surfaces, ids) = chart_polygons(&polygons.iter().collect::<Vec<_>>());
        assert_eq!(ids, vec![Some(0), None, Some(1)]);
        assert_eq!((surfaces[0].sides, surfaces[1].sides), (Sides::Two, Sides::One));
    }

    #[test]
    fn texels_stay_inside_on_a_slanted_plane() {
        let slanted = [ModelPolygon {
            normal: [0.0, -0.6, 0.8],
            ..polygon(4, 0, vec![[-30.0, 10.0, 7.5], [70.0, 10.0, 7.5], [70.0, 90.0, 67.5], [-30.0, 90.0, 67.5]])
        }];
        let (surfaces, _) = chart_polygons(&slanted.iter().collect::<Vec<_>>());
        assert!(inside(&surfaces[0], &slanted));
        assert!(Vec3::from(surfaces[0].normal).dot(Vec3::from(surfaces[0].u)).abs() < 1e-5);
    }
}
