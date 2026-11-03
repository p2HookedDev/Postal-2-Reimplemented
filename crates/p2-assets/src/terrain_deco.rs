use p2_math::{Rng, Rotator, Vector};
use p2_script::{Properties, StoredValue};

use crate::terrain::{Terrain, floats, member};

#[derive(Debug, Clone, Default)]
#[expect(clippy::struct_excessive_bools, reason = "the bools mirror the int flags of the retail script struct")]
pub struct DecoLayer {
    pub show_on_terrain: bool,
    pub scale_map: i32,
    pub density_map: i32,
    pub color_map: i32,
    pub static_mesh: i32,
    pub scale_multiplier: [[f32; 2]; 3],
    pub fadeout_radius: [f32; 2],
    pub density_multiplier: [f32; 2],
    pub max_per_quad: i32,
    pub seed: i32,
    pub align_to_terrain: bool,
    pub draw_order: u8,
    pub show_on_invisible_terrain: bool,
    pub lit_directional: bool,
    pub disregard_terrain_lighting: bool,
    pub random_yaw: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TexelMap<'a, T> {
    pub width: usize,
    pub height: usize,
    pub texels: &'a [T],
}

/// Texture data a layer's maps point to. A map the layer names but that is `None` here (unsupported format,
/// not loaded) samples as zero, as in the engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecoMaps<'a> {
    pub density: Option<TexelMap<'a, u8>>,
    /// `ScaleMap` and `ColorMap` texels as stored by RGBA8 textures (B, G, R, A); the engine reads no other
    /// format ([`crate::texture_bgra`]).
    pub scale: Option<TexelMap<'a, [u8; 4]>>,
    pub color: Option<TexelMap<'a, [u8; 4]>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub color: [u8; 4],
}

pub(crate) fn read_deco_layer(members: &Properties) -> DecoLayer {
    let int = |name| match member(members, name) {
        Some(&StoredValue::Int(v)) => v,
        _ => 0,
    };
    let object = |name| match member(members, name) {
        Some(&StoredValue::Object(v)) => v,
        _ => 0,
    };
    let range = |value: Option<&StoredValue>| floats(value, ["Min", "Max"], 0.0);
    let scale_multiplier = match member(members, "ScaleMultiplier") {
        Some(StoredValue::Struct(axes)) => ["X", "Y", "Z"].map(|axis| range(member(axes, axis))),
        _ => [[0.0; 2]; 3],
    };
    DecoLayer {
        show_on_terrain: int("ShowOnTerrain") != 0,
        scale_map: object("ScaleMap"),
        density_map: object("DensityMap"),
        color_map: object("ColorMap"),
        static_mesh: object("StaticMesh"),
        scale_multiplier,
        fadeout_radius: range(member(members, "FadeoutRadius")),
        density_multiplier: range(member(members, "DensityMultiplier")),
        max_per_quad: int("MaxPerQuad"),
        seed: int("Seed"),
        align_to_terrain: int("AlignToTerrain") != 0,
        draw_order: match member(members, "DrawOrder") {
            Some(&StoredValue::Byte(v)) => v,
            _ => 0,
        },
        show_on_invisible_terrain: int("ShowOnInvisibleTerrain") != 0,
        lit_directional: int("LitDirectional") != 0,
        disregard_terrain_lighting: int("DisregardTerrainLighting") != 0,
        random_yaw: int("RandomYaw") != 0,
    }
}

/// A 32-bit LCG as retail: the low 23 bits become the mantissa of a float in [1, 2), minus 1.
struct SRand(u32);

impl SRand {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(0x0bb3_8435).wrapping_add(0x3619_636b);
        f32::from_bits(0x3f80_0000 | (self.0 & 0x007f_ffff)) - 1.0
    }

    /// `max + (min - max) * r`, kept in x87 precision for the caller (modelled as `f64`).
    fn range(&mut self, [min, max]: [f32; 2]) -> f64 {
        let r = self.next();
        (f64::from(min) - f64::from(max)) * f64::from(r) + f64::from(max)
    }

    /// One draw per axis in the order Z, Y, X.
    fn range_vector(&mut self, ranges: &[[f32; 2]; 3]) -> [f32; 3] {
        let mut out = [0.0; 3];
        for axis in [2, 1, 0] {
            let [min, max] = ranges[axis];
            let r = self.next();
            out[axis] = (f64::from(r) * f64::from(min - max) + f64::from(max)) as f32;
        }
        out
    }
}

impl Terrain {
    pub fn decorations(&self, layer: &DecoLayer, vertices: &[[f32; 3]], maps: &DecoMaps) -> Vec<Decoration> {
        let mut out = Vec::new();
        let (hx, hy) = (self.width, self.height);
        if layer.static_mesh == 0 || layer.density_map == 0 || vertices.len() != hx * hy {
            return out;
        }
        let mut placing = Placing { layer, vertices, maps, rng: SRand(0), crt: Rng::new(1) };
        for (index, [ox, oy, qx, qy]) in self.sectors().into_iter().enumerate() {
            placing.rng = SRand(layer.seed.wrapping_add(index as i32) as u32);
            for y in 0..qy {
                for x in 0..qx {
                    let (gx, gy) = (ox + x, oy + y);
                    if !layer.show_on_invisible_terrain && !self.quad_visible(gx, gy) {
                        continue;
                    }
                    for _ in 0..layer.max_per_quad {
                        let alpha = maps.density.map_or(0, |map| sample(map, gx, gy, hx, hy)) as f32 / 255.0;
                        let r = placing.rng.next();
                        let density = (placing.rng.range(layer.density_multiplier) * f64::from(alpha)) as f32;
                        if density <= r {
                            continue;
                        }
                        out.extend(placing.place(self, gx, gy));
                    }
                }
            }
        }
        out
    }

    pub fn sectors(&self) -> Vec<[usize; 4]> {
        let size = self.sector_size;
        if size == 0 {
            return Vec::new();
        }
        let (nx, ny) = (self.width / size, self.height / size);
        let quads = |i: usize, n: usize| if i + 1 >= n { size - 1 } else { size };
        (0..ny).flat_map(|sy| (0..nx).map(move |sx| [sx * size, sy * size, quads(sx, nx), quads(sy, ny)])).collect()
    }
}

struct Placing<'a> {
    layer: &'a DecoLayer,
    vertices: &'a [[f32; 3]],
    maps: &'a DecoMaps<'a>,
    rng: SRand,
    /// Stand-in for the engine's random (default seed 1). Its generator is shared with the rest of the
    /// game and never reseeded here, so `RandomYaw` angles cannot be reproduced.
    crt: Rng,
}

impl Placing<'_> {
    fn place(&mut self, terrain: &Terrain, gx: usize, gy: usize) -> Option<Decoration> {
        let Self { layer, vertices, maps, rng, crt } = self;
        let (hx, hy) = (terrain.width, terrain.height);
        let v = |x: usize, y: usize| vertices.get(y * hx + x).copied().map(Vector::from);
        let (v00, v10, v01, v11) = (v(gx, gy)?, v(gx + 1, gy)?, v(gx, gy + 1)?, v(gx + 1, gy + 1)?);
        let a = rng.next();
        let b = rng.next();
        // A point in one of the two triangles split along v00-v11 (whatever the quad's EdgeTurn).
        let (base, e1, e2, k1, k2) =
            if a > b { (v10, v00 - v10, v11 - v10, 1.0 - a, b) } else { (v01, v00 - v01, v11 - v01, a, 1.0 - b) };
        let location: [f32; 3] = std::array::from_fn(|i| (base[i] + e1[i] * k1) + e2[i] * k2);

        let mut normal = safe_normal(e1.cross(e2).into());
        if normal[2] < 0.0 {
            normal = normal.map(|c| -c);
        }
        if terrain.inverted {
            normal = normal.map(|c| -c);
        }
        let up = if layer.align_to_terrain { normal } else { [0.0, 0.0, 1.0] };
        let Rotator { pitch, yaw, roll } = Vector::from(up).rotation();
        let mut rotation = [pitch - 0x4000, yaw, roll];

        let mut scale = rng.range_vector(&layer.scale_multiplier);
        if layer.random_yaw {
            rotation[1] += crt.rand() % 0x10000;
        }
        if layer.scale_map != 0 {
            let [b, g, r, _] = maps.scale.map_or([0; 4], |map| sample(map, gx, gy, hx, hy));
            for (s, c) in scale.iter_mut().zip([r, g, b]) {
                *s *= f32::from(c) / 255.0;
            }
        }
        let mut color = match terrain.vertex_colors.get(gy * hx + gx) {
            Some(&lit) if !layer.disregard_terrain_lighting => lit,
            Some(_) => [127, 127, 127, 255],
            None => [255; 4],
        };
        if layer.color_map != 0 {
            let [b, g, r, _] = maps.color.map_or([0; 4], |map| sample(map, gx, gy, hx, hy));
            for (channel, c) in color.iter_mut().zip([r, g, b]) {
                *channel = (f32::from(*channel) * (f32::from(c) / 255.0)) as i32 as u8;
            }
        }
        Some(Decoration { location: location.into(), rotation: rotation.into(), scale: scale.into(), color })
    }
}

fn sample<T: Copy + Default>(map: TexelMap<T>, x: usize, y: usize, hx: usize, hy: usize) -> T {
    let (u, v) = (map.width * x / hx.max(1), map.height * y / hy.max(1));
    if u >= map.width || v >= map.height {
        return T::default();
    }
    map.texels.get(v * map.width + u).copied().unwrap_or_default()
}

fn safe_normal(v: [f32; 3]) -> [f32; 3] {
    let squared = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if f64::from(squared) < 1e-8 {
        return [0.0; 3];
    }
    let length = f64::from(squared).sqrt() as f32;
    let scale = (1.0 / f64::from(length)) as f32;
    v.map(|c| c * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srand_is_mantissa_of_lcg() {
        let mut rng = SRand(0);
        assert_eq!(rng.next(), f32::from_bits(0x3f80_0000 | 0x19_636b) - 1.0);
        let values: Vec<f32> = (0..1000).map(|_| rng.next()).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
    }

    #[test]
    fn upright_rotation_has_pitch_minus_one() {
        assert_eq!(Vector::new(0.0, 0.0, 1.0).rotation(), Rotator::new(16383, 0, 0));
    }

    #[test]
    fn range_vector_draws_z_first() {
        let mut a = SRand(7);
        let scale = a.range_vector(&[[1.0, 1.0], [2.0, 2.0], [0.0, 1.0]]);
        let mut b = SRand(7);
        let r = b.next();
        assert_eq!(scale, [1.0, 2.0, 1.0 - r]);
    }
}
