use p2_math::Vector;
use p2_package::{Export, Package, Reader};
use p2_script::{Properties, StructLayout, StoredValue, read_properties};

use crate::Error;
use crate::read::{array, invalid, skip_array};
use crate::terrain_deco::{DecoLayer, read_deco_layer};

#[derive(Debug, Clone)]
pub struct Terrain {
    pub width: usize,
    pub height: usize,
    pub heightmap: i32,
    pub scale: Vector,
    pub location: Vector,
    /// World-space vertex of heightmap texel (x, y) at `y * width + x`, as saved; empty when the saved
    /// vertices do not fit the heightmap size (rebuild them with [`Terrain::vertices_from_heights`]).
    pub vertices: Vec<[f32; 3]>,
    pub vertex_colors: Vec<[u8; 4]>,
    /// One bit per quad (`y * width + x`): set = drawn. Empty = every quad drawn.
    pub quad_visibility: Vec<i32>,
    /// One bit per quad: set = the quad's diagonal runs the other way.
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
    /// `TerrainMatrix` rows (`XPlane`, `YPlane`, `ZPlane`, `WPlane`): texture UV = (x, y, z, 1) · rows, first two columns.
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

    /// The visible quads' triangles as Karma collides with them: [`Terrain::triangles`] reversed, so each
    /// one's right-handed normal ((b − a) × (c − b)) points up for a positive `TerrainScale`.
    pub fn karma_triangles(&self) -> Vec<[usize; 3]> {
        self.triangles().into_iter().map(|[a, b, c]| [a, c, b]).collect()
    }

    pub fn quad_triangles(&self, x: usize, y: usize) -> Vec<[usize; 3]> {
        let quad = y * self.width + x;
        if x + 1 >= self.width || y + 1 >= self.height || !self.quad_visible(x, y) {
            return Vec::new();
        }
        let (v00, v10, v01, v11) = (quad, quad + 1, quad + self.width, quad + self.width + 1);
        if bit(&self.edge_turn, quad) {
            vec![[v00, v01, v10], [v01, v11, v10]]
        } else {
            vec![[v00, v01, v11], [v00, v11, v10]]
        }
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

    pub fn alpha_uv(&self, index: usize) -> [f32; 2] {
        let (x, y) = (index % self.width.max(1), index / self.width.max(1));
        [(x as f32 + 0.5) / self.width as f32, (y as f32 + 0.5) / self.height as f32]
    }

    /// Cross product of the steps across a vertex along rows and columns (one-sided at the edges); up for
    /// a positive `TerrainScale`, down when `Inverted`. The engine's exact neighbours are (unverified).
    pub fn vertex_normals(&self) -> Vec<[f32; 3]> {
        let (w, h) = (self.width.max(1), self.height.max(1));
        let at = |x: usize, y: usize| self.vertices.get(y * w + x).copied().map_or(Vector::ZERO, Vector::from);
        let sign = if self.inverted { -1.0 } else { 1.0 };
        (0..self.vertices.len())
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let along = at((x + 1).min(w - 1), y) - at(x.saturating_sub(1), y);
                let across = at(x, (y + 1).min(h - 1)) - at(x, y.saturating_sub(1));
                let n = along.cross(across);
                let length = n.dot(n).sqrt();
                if length > 0.0 { <[f32; 3]>::from(n).map(|c| c * sign / length) } else { [0.0, 0.0, sign] }
            })
            .collect()
    }
}

/// Most elements a per-element tag of `Layers` / `DecoLayers` may name: far above the 32 layers `TerrainInfo`
/// declares, so a damaged index cannot allocate without bound.
const MAX_LAYER_TAGS: usize = 256;
/// Largest heightmap side read: engine textures stop at 2048 and retail terrains are far smaller (unverified), while
/// `triangles` and `sectors` loop over width x height, so a damaged size must not get near 2^31.
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

    // A bad count must not cost the terrain and its collision, only some decorations.
    for layer in &mut deco_layers {
        let layer: &mut DecoLayer = layer;
        layer.max_per_quad = layer.max_per_quad.min(MAX_DECO_PER_QUAD);
    }

    let r = &mut r;
    array(r, |r| Ok(r.compact()?))?;
    let vertices = array(r, |r| Ok([r.f32()?, r.f32()?, r.f32()?]))?;
    r.bytes(8)?;
    skip_array(r, 24)?;
    r.bytes(2 * 12 * 4)?; // ToWorld, ToHeightmap: 12 floats each
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
    fn slope_normals_lean_away_from_the_rise() {
        let half = std::f32::consts::FRAC_1_SQRT_2;
        for [x, y, z] in terrain(|x, _| x).vertex_normals() {
            assert!((x + half).abs() < 1e-5 && y.abs() < 1e-5 && (z - half).abs() < 1e-5, "{x} {y} {z}");
        }
    }
}
