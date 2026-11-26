use p2_math::Vector;
use p2_package::{Export, Package, Reader};
use p2_script::{Properties, StructLayout, StoredValue, read_properties};

use crate::Error;
use crate::read::{array, invalid, skip_array, vec3};
use crate::terrain_deco::{DecoLayer, read_deco_layer};

#[derive(Debug, Clone)]
pub struct Terrain {
    pub width: usize,
    pub height: usize,
    pub heightmap: i32,
    pub scale: Vector,
    pub location: Vector,
    pub vertices: Vec<[f32; 3]>,
    pub vertex_colors: Vec<[u8; 4]>,
    pub quad_visibility: Vec<i32>,
    pub edge_turn: Vec<i32>,
    pub layers: Vec<TerrainLayer>,
    pub sector_size: usize,
    pub inverted: bool,
    pub deco_layers: Vec<DecoLayer>,
}

#[derive(Debug, Clone, Default)]
pub struct TerrainLayer {
    pub texture: i32,
    pub alpha_map: i32,
    pub matrix: [[f32; 4]; 4],
}

impl Terrain {
    /// World positions from heightmap values (`width * height`, row-major), as the engine builds them:
    /// `((x - width / 2) * scale.x, (y - height / 2) * scale.y, (h - 32767) * scale.z / 256) + location`.
    /// Rotation and draw scale of the actor do not apply.
    pub fn vertices_from_heights(&self, heights: &[u16]) -> Vec<[f32; 3]> {
        let Vector { x: sx, y: sy, z: sz } = self.scale;
        let Vector { x: lx, y: ly, z: lz } = self.location;
        let (half_x, half_y) = ((self.width / 2) as f32, (self.height / 2) as f32);
        heights
            .iter()
            .enumerate()
            .map(|(i, &h)| {
                let (x, y) = ((i % self.width.max(1)) as f32, (i / self.width.max(1)) as f32);
                [(x - half_x) * sx + lx, (y - half_y) * sy + ly, (f32::from(h) - 32767.0) * sz / 256.0 + lz]
            })
            .collect()
    }

    pub fn triangles(&self) -> Vec<[usize; 3]> {
        let mut triangles = Vec::new();
        for y in 0..self.height.saturating_sub(1) {
            for x in 0..self.width.saturating_sub(1) {
                triangles.extend(self.quad_triangles(x, y));
            }
        }
        triangles
    }

    pub fn karma_triangles(&self) -> Vec<[usize; 3]> {
        if self.inverted {
            return self.triangles();
        }
        self.triangles().into_iter().map(|[a, b, c]| [a, c, b]).collect()
    }

    pub fn quad_triangles(&self, x: usize, y: usize) -> std::iter::Take<std::array::IntoIter<[usize; 3], 2>> {
        let quad = y * self.width + x;
        if x + 1 >= self.width || y + 1 >= self.height || !self.quad_visible(x, y) {
            return [[0; 3]; 2].into_iter().take(0);
        }
        let (v00, v10, v01, v11) = (quad, quad + 1, quad + self.width, quad + self.width + 1);
        let triangles = if bit(&self.edge_turn, quad) { [[v00, v01, v10], [v01, v11, v10]] } else { [[v00, v01, v11], [v00, v11, v10]] };
        triangles.into_iter().take(2)
    }

    pub fn quad_visible(&self, x: usize, y: usize) -> bool {
        self.quad_visibility.is_empty() || bit(&self.quad_visibility, y * self.width + x)
    }

    pub fn layer_uvs(&self, layer: &TerrainLayer) -> Vec<[f32; 2]> {
        let m = &layer.matrix;
        self.vertices
            .iter()
            .map(|&[x, y, z]| {
                let column = |c: usize| x * m[0][c] + y * m[1][c] + z * m[2][c] + m[3][c];
                [column(0), column(1)]
            })
            .collect()
    }

    pub fn subdivided(&self, n: usize) -> Self {
        let (w, h) = (self.width, self.height);
        if n < 2 || w < 2 || h < 2 || self.vertices.len() != w * h {
            return self.clone();
        }
        let height = |x: isize, y: isize| self.vertices[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize][2];
        let curve = |[a, b, c, d]: [f32; 4], t: f32| {
            0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t + (3.0 * b - a - 3.0 * c + d) * t * t * t)
        };
        let curved = |x: usize, y: usize| {
            let inside = x > 0 && y > 0 && x < w - 1 && y < h - 1;
            let shown = [(1, 1), (0, 1), (1, 0), (0, 0)].iter().all(|&(dx, dy)| inside && self.quad_visible(x - dx, y - dy));
            if shown { 1.0 } else { 0.0 }
        };
        let bilinear = |[c00, c10, c01, c11]: [f32; 4], tx: f32, ty: f32| (c00 + (c10 - c00) * tx) * (1.0 - ty) + (c01 + (c11 - c01) * tx) * ty;
        let (fine_w, fine_h) = ((w - 1) * n + 1, (h - 1) * n + 1);
        let mut vertices = Vec::with_capacity(fine_w * fine_h);
        let mut vertex_colors = Vec::with_capacity(if self.vertex_colors.is_empty() { 0 } else { fine_w * fine_h });
        let mut bits = [&self.quad_visibility, &self.edge_turn].map(|coarse| vec![0_i32; if coarse.is_empty() { 0 } else { (fine_w * fine_h).div_ceil(32) }]);
        for fy in 0..fine_h {
            let qy = (fy / n).min(h - 2);
            let ty = (fy - qy * n) as f32 / n as f32;
            for fx in 0..fine_w {
                let qx = (fx / n).min(w - 2);
                let tx = (fx - qx * n) as f32 / n as f32;
                let quad = qy * w + qx;
                let corners = [quad, quad + 1, quad + w, quad + w + 1];
                let [z00, z10, z01, z11] = corners.map(|i| self.vertices[i][2]);
                let flat = match (bit(&self.edge_turn, quad), tx + ty < 1.0, tx > ty) {
                    (true, true, _) => z00 + tx * (z10 - z00) + ty * (z01 - z00),
                    (true, false, _) => z11 + (1.0 - tx) * (z01 - z11) + (1.0 - ty) * (z10 - z11),
                    (false, _, true) => z00 + tx * (z10 - z00) + ty * (z11 - z10),
                    (false, _, false) => z00 + ty * (z01 - z00) + tx * (z11 - z01),
                };
                let (x, y) = (qx as isize, qy as isize);
                let rows = [-1, 0, 1, 2].map(|j| curve([-1, 0, 1, 2].map(|i| height(x + i, y + j)), tx));
                let weight = bilinear([curved(qx, qy), curved(qx + 1, qy), curved(qx, qy + 1), curved(qx + 1, qy + 1)], tx, ty);
                let [x0, y0, _] = self.vertices[quad];
                let [x1, y1, _] = self.vertices[quad + w + 1];
                vertices.push([x0 + (x1 - x0) * tx, y0 + (y1 - y0) * ty, flat + (curve(rows, ty) - flat) * weight]);
                if !self.vertex_colors.is_empty() {
                    let channel = |c: usize| bilinear(corners.map(|i| f32::from(self.vertex_colors[i][c])), tx, ty).round() as u8;
                    vertex_colors.push([0, 1, 2, 3].map(channel));
                }
                let fine = fy * fine_w + fx;
                for (bits, coarse) in bits.iter_mut().zip([&self.quad_visibility, &self.edge_turn]) {
                    if bit(coarse, quad) && !bits.is_empty() {
                        bits[fine / 32] |= 1 << (fine % 32);
                    }
                }
            }
        }
        let [quad_visibility, edge_turn] = bits;
        Self {
            width: fine_w,
            height: fine_h,
            scale: Vector::new(self.scale.x / n as f32, self.scale.y / n as f32, self.scale.z),
            vertices,
            vertex_colors,
            quad_visibility,
            edge_turn,
            sector_size: self.sector_size * n,
            ..self.clone()
        }
    }

    pub fn vertex_normals(&self) -> Vec<[f32; 3]> {
        let (w, h) = (self.width, self.height);
        if self.vertices.len() != w * h {
            return Vec::new();
        }
        let face = |a: usize, b: usize, c: usize| {
            let [a, b, c] = [a, b, c].map(|i| Vector::from(self.vertices[i]));
            (b - a).cross(c - a).safe_normal()
        };
        let mut sums = vec![Vector::ZERO; self.vertices.len()];
        for y in 0..h.saturating_sub(1) {
            for x in 0..w.saturating_sub(1) {
                let quad = y * w + x;
                let (v00, v10, v01, v11) = (quad, quad + 1, quad + w, quad + w + 1);
                let normal =
                    if bit(&self.edge_turn, quad) { face(v00, v10, v01) + face(v01, v10, v11) } else { face(v00, v10, v11) + face(v00, v11, v01) };
                for corner in [v00, v10, v01, v11] {
                    sums[corner] += normal;
                }
            }
        }
        let sign = if self.inverted { -1.0 } else { 1.0 };
        sums.into_iter().map(|sum| (sum.safe_normal() * sign).into()).collect()
    }
}

const MAX_LAYER_TAGS: usize = 256;
/// Largest heightmap side read: engine textures stop at 2048 and retail terrains are far smaller (unverified),
/// while `triangles` and `sectors` loop over width x height, so a damaged size must not get near 2^31.
const MAX_TERRAIN_SIZE: usize = 4096;
const MAX_DECO_PER_QUAD: i32 = 1024;

pub fn read_terrain(pkg: &Package, file: &[u8], export: &Export, layout: &StructLayout) -> Result<Terrain, Error> {
    let mut r = pkg.export_reader(file, export)?;
    let properties = read_properties(pkg, &mut r, layout)?;
    let mut layers = Vec::new();
    let (mut quad_visibility, mut edge_turn) = (Vec::new(), Vec::new());
    let (mut heightmap, mut scale, mut location) = (0, Vector::new(64.0, 64.0, 64.0), Vector::ZERO);
    let (mut sector_size, mut inverted, mut deco_layers) = (16, false, Vec::new());
    for property in &properties.values {
        match (property.name.to_ascii_lowercase().as_str(), &property.value) {
            ("terrainmap", &StoredValue::Object(map)) => heightmap = map,
            ("terrainsectorsize", &StoredValue::Int(size)) => sector_size = usize::try_from(size).unwrap_or(0),
            ("inverted", &StoredValue::Bool(value)) => inverted = value,
            ("decolayers", StoredValue::Array(values)) => {
                deco_layers = values.iter().filter_map(|v| if let StoredValue::Struct(m) = v { Some(read_deco_layer(m)) } else { None }).collect();
            }
            ("decolayers", StoredValue::Struct(members)) if (property.array_index as usize) < MAX_LAYER_TAGS => {
                *element(&mut deco_layers, property.array_index as usize) = read_deco_layer(members);
            }
            ("terrainscale", value) => scale = floats(Some(value), ["X", "Y", "Z"], 64.0).into(),
            ("location", value) => location = floats(Some(value), ["X", "Y", "Z"], 0.0).into(),
            ("layers", StoredValue::Struct(members)) if (property.array_index as usize) < MAX_LAYER_TAGS => {
                *element(&mut layers, property.array_index as usize) = read_layer(members);
            }
            ("quadvisibilitybitmap", StoredValue::Array(values)) => quad_visibility = ints(values),
            ("edgeturnbitmap", StoredValue::Array(values)) => edge_turn = ints(values),
            _ => {}
        }
    }

    for layer in &mut deco_layers {
        let layer: &mut DecoLayer = layer;
        layer.max_per_quad = layer.max_per_quad.min(MAX_DECO_PER_QUAD);
    }

    let r = &mut r;
    array(r, |r| Ok(r.compact()?))?;
    let vertices = array(r, vec3)?;
    r.bytes(8)?;
    skip_array(r, 24)?;
    r.bytes(2 * 12 * 4)?;
    let width = dimension(r)?;
    let height = dimension(r)?;
    let vertex_colors = array(r, |r| Ok(r.array::<4>()?))?;
    if !r.at_end() {
        return Err(invalid("terrain end", r.pos()));
    }
    // Saved vertices can be stale (e.g. MPDGT-Trainyard: 256² vertices for a 64² heightmap); the engine
    // rebuilds them from the heightmap on load.
    let vertices = if vertices.len() == width * height { vertices } else { Vec::new() };
    let vertex_colors = if vertex_colors.len() == width * height { vertex_colors } else { Vec::new() };
    Ok(Terrain {
        width,
        height,
        heightmap,
        scale,
        location,
        vertices,
        vertex_colors,
        quad_visibility,
        edge_turn,
        layers,
        sector_size,
        inverted,
        deco_layers,
    })
}

fn element<T: Default>(items: &mut Vec<T>, index: usize) -> &mut T {
    if items.len() <= index {
        items.resize_with(index + 1, T::default);
    }
    &mut items[index]
}

fn read_layer(members: &Properties) -> TerrainLayer {
    let mut layer = TerrainLayer::default();
    for property in &members.values {
        match (property.name.to_ascii_lowercase().as_str(), &property.value) {
            ("texture", &StoredValue::Object(texture)) => layer.texture = texture,
            ("alphamap", &StoredValue::Object(alpha_map)) => layer.alpha_map = alpha_map,
            ("terrainmatrix", StoredValue::Struct(planes)) => {
                for (row, plane) in ["XPlane", "YPlane", "ZPlane", "WPlane"].into_iter().enumerate() {
                    layer.matrix[row] = floats(member(planes, plane), ["X", "Y", "Z", "W"], 0.0);
                }
            }
            _ => {}
        }
    }
    layer
}

fn bit(bits: &[i32], i: usize) -> bool {
    bits.get(i / 32).is_some_and(|word| word >> (i % 32) & 1 != 0)
}

pub(crate) fn member<'v>(members: &'v Properties, name: &str) -> Option<&'v StoredValue> {
    members.values.iter().find(|p| p.array_index == 0 && p.name.eq_ignore_ascii_case(name)).map(|p| &p.value)
}

pub(crate) fn floats<const N: usize>(value: Option<&StoredValue>, names: [&str; N], default: f32) -> [f32; N] {
    let Some(StoredValue::Struct(members)) = value else { return [default; N] };
    names.map(|name| match member(members, name) {
        Some(&StoredValue::Float(value)) => value,
        _ => default,
    })
}

fn ints(values: &[StoredValue]) -> Vec<i32> {
    values.iter().map(|value| if let &StoredValue::Int(v) = value { v } else { 0 }).collect()
}

fn dimension(r: &mut Reader) -> Result<usize, Error> {
    let offset = r.pos();
    usize::try_from(r.i32()?).ok().filter(|&size| size <= MAX_TERRAIN_SIZE).ok_or_else(|| invalid("heightmap size", offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terrain(z: impl Fn(f32, f32) -> f32) -> Terrain {
        let vertices = (0..9).map(|i| ((i % 3) as f32, (i / 3) as f32)).map(|(x, y)| [x, y, z(x, y)]).collect();
        Terrain {
            width: 3,
            height: 3,
            heightmap: 0,
            scale: Vector::new(1.0, 1.0, 1.0),
            location: Vector::ZERO,
            vertices,
            vertex_colors: Vec::new(),
            quad_visibility: Vec::new(),
            edge_turn: Vec::new(),
            layers: Vec::new(),
            sector_size: 16,
            inverted: false,
            deco_layers: Vec::new(),
        }
    }

    #[test]
    fn oversized_heightmap_is_rejected() {
        let too_big = i32::try_from(MAX_TERRAIN_SIZE + 1).unwrap().to_le_bytes();
        assert!(dimension(&mut Reader::new(&too_big)).is_err());
        assert_eq!(dimension(&mut Reader::new(&64_i32.to_le_bytes())).unwrap(), 64);
    }

    #[test]
    fn flat_terrain_normals_point_up() {
        assert!(terrain(|_, _| 0.0).vertex_normals().iter().all(|&n| n == [0.0, 0.0, 1.0]));
        let inverted = Terrain { inverted: true, ..terrain(|_, _| 0.0) };
        assert!(inverted.vertex_normals().iter().all(|&n| n == [0.0, 0.0, -1.0]));
    }

    #[test]
    fn subdivision_keeps_the_vertices_and_rounds_between_them() {
        let bump = |x: f32, y: f32| if (x, y) == (2.0, 2.0) { 8.0 } else { 0.0 };
        let vertices = (0..25).map(|i| ((i % 5) as f32, (i / 5) as f32)).map(|(x, y)| [x, y, bump(x, y)]).collect();
        let coarse = Terrain { width: 5, height: 5, vertices, ..terrain(|_, _| 0.0) };
        let fine = coarse.subdivided(2);
        assert_eq!((fine.width, fine.height, fine.sector_size), (9, 9, 32));
        for (i, vertex) in coarse.vertices.iter().enumerate() {
            assert_eq!(fine.vertices[(i / 5) * 2 * 9 + (i % 5) * 2], *vertex);
        }
        let flat_middle = 4.0;
        assert!(fine.vertices[4 * 9 + 3][2] > flat_middle, "{:?}", fine.vertices[4 * 9 + 3]);
        assert_eq!(coarse.subdivided(1).vertices, coarse.vertices);
    }

    #[test]
    fn subdivision_leaves_the_quads_around_a_hole_flat() {
        let vertices: Vec<[f32; 3]> = (0..25).map(|i| ((i % 5) as f32, (i / 5) as f32)).map(|(x, y)| [x, y, x * x]).collect();
        let mut quad_visibility = vec![-1_i32];
        quad_visibility[0] &= !(1 << (5 + 1));
        let holed = Terrain { width: 5, height: 5, vertices, quad_visibility, ..terrain(|_, _| 0.0) };
        let fine = holed.subdivided(2);
        assert!(!fine.quad_visible(2, 2) && !fine.quad_visible(3, 3) && fine.quad_visible(4, 2));
        assert_eq!(fine.vertices[2 * 9 + 3][2], 2.5);
        assert_eq!(fine.vertices[4][2], 4.0);
    }

    #[test]
    fn slope_normals_lean_away_from_the_rise() {
        let half = std::f32::consts::FRAC_1_SQRT_2;
        for [x, y, z] in terrain(|x, _| x).vertex_normals() {
            assert!((x + half).abs() < 1e-5 && y.abs() < 1e-5 && (z - half).abs() < 1e-5, "{x} {y} {z}");
        }
    }
}
