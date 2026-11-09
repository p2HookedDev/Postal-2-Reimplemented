//! Relights a map's BSP from its own lights: finer texels, soft shadows from the world, one bounce and ambient
//! occlusion. Atlases keep retail's layout scaled by [`SCALE`], so the BSP's texture coordinates stay valid, and alpha
//! holds how much of the shadow sun reached each texel, as the retail sun mask does.

mod cache;
mod scene;

use std::f32::consts::TAU;
use std::time::Instant;

use p2_assets::{LightMap, Model, Terrain};
use p2_collision::MeshCollision;
use p2_math::Vector;
use p2_render_cpu::{LE_SUNLIGHT, MeshLight};
use rayon::prelude::*;

pub use cache::{cache_key, load, save};
pub use scene::is_sky;
use scene::{Scene, Seen};

pub const SCALE: usize = 2;

const LIFT: f32 = 2.0;
const SUN_ANGLE: f32 = 0.03;
const SUN_SAMPLES: usize = 8;
const LAMP_RADIUS: f32 = 12.0;
const LAMP_SAMPLES: usize = 4;
const INDIRECT_RAYS: usize = 32;
const BOUNCE_DISTANCE: f32 = 2048.0;
const AO_DISTANCE: f32 = 96.0;
const AO_DARKEST: f32 = 0.35;
// Retail's lamps stand in for indirect light, so they take part of the occlusion too. The sun does not.
const LAMP_AO: f32 = 0.5;
const BOUNCE_ALBEDO: f32 = 0.45;
const SMALLEST_TERM: f32 = 1.0 / 512.0;

pub struct BakeInput<'a> {
    pub model: &'a Model,
    pub terrains: &'a [Terrain],
    pub meshes: &'a [MeshCollision],
    pub lights: &'a [MeshLight],
    pub shadow_sun: Option<usize>,
    pub zone_ambients: &'a [[f32; 3]],
    pub level_brightness: f32,
}

#[derive(Debug, Clone)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub texels: Vec<[u8; 4]>,
}

pub struct Baked {
    pub atlases: Vec<Option<Atlas>>,
    pub texels: usize,
    pub millis: u128,
    /// Display-space means over the texels, to judge how far the relight drifts from retail.
    pub mean_retail: f32,
    pub mean_direct: f32,
    pub mean_final: f32,
}

pub fn lumel_positions(map: &LightMap) -> Option<impl Fn(f32, f32) -> Vector + use<>> {
    let [u0, v0] = map.lumel(map.base, 1.0);
    let [ux, vx] = map.lumel(map.axis_x, 0.0);
    let [uy, vy] = map.lumel(map.axis_y, 0.0);
    let det = ux * vy - uy * vx;
    if det.abs() < 1e-12 {
        return None;
    }
    let (base, axis_x, axis_y) = (Vector::from(map.base), Vector::from(map.axis_x), Vector::from(map.axis_y));
    Some(move |u: f32, v: f32| {
        let (du, dv) = (u - u0, v - v0);
        let (a, b) = ((du * vy - uy * dv) / det, (ux * dv - du * vx) / det);
        base + axis_x * a + axis_y * b
    })
}

/// Where each texel samples the surface, in lumels. Texels just outside a polygon sample its nearest edge, so the
/// filtering at its border never reads a texel without light.
struct Grid {
    width: usize,
    height: usize,
    scale: usize,
    samples: Vec<Option<[f32; 2]>>,
}

impl Grid {
    fn new(map: &LightMap, polygons: &[Vec<[f32; 2]>], scale: usize) -> Self {
        let (width, height) = (map.size[0].max(0) as usize * scale, map.size[1].max(0) as usize * scale);
        let mut samples = vec![None; width * height];
        let mut gaps = vec![f32::MAX; width * height];
        let texel = 1.0 / scale as f32;
        for polygon in polygons {
            let Some(shape) = ConvexShape::new(polygon) else { continue };
            let lowest = |axis: usize| polygon.iter().map(|p| p[axis]).fold(f32::MAX, f32::min);
            let highest = |axis: usize| polygon.iter().map(|p| p[axis]).fold(f32::MIN, f32::max);
            let span = |low: f32, high: f32, size: usize| {
                let first = ((low * scale as f32).floor() as isize - 1).max(0) as usize;
                let last = ((high * scale as f32).ceil() as isize + 1).clamp(0, size as isize) as usize;
                first..last
            };
            for y in span(lowest(1), highest(1), height) {
                for x in span(lowest(0), highest(0), width) {
                    let centre = [(x as f32 + 0.5) * texel, (y as f32 + 0.5) * texel];
                    let (nearest, gap) = shape.nearest(centre);
                    let at = y * width + x;
                    if gap <= texel && gap < gaps[at] {
                        gaps[at] = gap;
                        samples[at] = Some(nearest);
                    }
                }
            }
        }
        Self { width, height, scale, samples }
    }
}

struct ConvexShape<'a> {
    points: &'a [[f32; 2]],
    winding: f32,
    centre: [f32; 2],
}

impl<'a> ConvexShape<'a> {
    fn new(points: &'a [[f32; 2]]) -> Option<Self> {
        let mut area = 0.0;
        let mut centre = [0.0; 2];
        for (index, a) in points.iter().enumerate() {
            let b = points[(index + 1) % points.len()];
            area += a[0] * b[1] - b[0] * a[1];
            centre = [centre[0] + a[0], centre[1] + a[1]];
        }
        if area.abs() < 1e-6 {
            return None;
        }
        let count = points.len() as f32;
        Some(Self { points, winding: area.signum(), centre: [centre[0] / count, centre[1] / count] })
    }

    fn nearest(&self, point: [f32; 2]) -> ([f32; 2], f32) {
        let mut inside = true;
        let mut best = (point, f32::MAX);
        for (index, a) in self.points.iter().enumerate() {
            let b = self.points[(index + 1) % self.points.len()];
            let edge = [b[0] - a[0], b[1] - a[1]];
            let to_point = [point[0] - a[0], point[1] - a[1]];
            if (edge[0] * to_point[1] - edge[1] * to_point[0]) * self.winding < 0.0 {
                inside = false;
            }
            let length = (edge[0] * edge[0] + edge[1] * edge[1]).max(1e-12);
            let along = ((to_point[0] * edge[0] + to_point[1] * edge[1]) / length).clamp(0.0, 1.0);
            let on_edge = [a[0] + edge[0] * along, a[1] + edge[1] * along];
            let gap = (point[0] - on_edge[0]).hypot(point[1] - on_edge[1]);
            if gap < best.1 {
                best = (on_edge, gap);
            }
        }
        if inside {
            return (point, 0.0);
        }
        // Just inside the edge: exactly on it the lifted ray can start in the neighbouring wall.
        let (edge, gap) = best;
        ([edge[0] + (self.centre[0] - edge[0]) * 0.01, edge[1] + (self.centre[1] - edge[1]) * 0.01], gap)
    }
}

/// A fixed sequence per texel, so a bake is repeatable and neighbours do not share a noise pattern.
fn jitter(x: usize, y: usize, sample: usize) -> [f32; 2] {
    let (x, y) = (x as f32, y as f32);
    let seed = (52.98292 * (0.06711056 * x + 0.00583715 * y).fract()).fract();
    let other = (52.98292 * (0.00583715 * x + 0.06711056 * y).fract()).fract();
    [(seed + sample as f32 * 0.754_877_7).fract(), (other + sample as f32 * 0.569_840_3).fract()]
}

fn axes_of(normal: Vector) -> [Vector; 2] {
    let side = if normal.z.abs() > 0.9 { Vector::new(1.0, 0.0, 0.0) } else { Vector::new(0.0, 0.0, 1.0) };
    let first = normal.cross(side).safe_normal();
    [first, normal.cross(first)]
}

fn on_disk([a, b]: [f32; 2]) -> [f32; 2] {
    let (radius, angle) = (a.sqrt(), b * TAU);
    [radius * angle.cos(), radius * angle.sin()]
}

struct Surface<'a> {
    map: &'a LightMap,
    normal: Vector,
    polygons: Vec<Vec<[f32; 2]>>,
}

/// Lamps and suns apart: only lamps take part of the occlusion.
struct Direct {
    grid: Grid,
    lamps: Vec<[f32; 3]>,
    suns: Vec<[f32; 3]>,
    shadow_sun: Vec<f32>,
}

impl Direct {
    fn light_at(&self, [u, v]: [f32; 2]) -> [f32; 3] {
        let (x, y) = ((u * self.grid.scale as f32) as usize, (v * self.grid.scale as f32) as usize);
        if x >= self.grid.width || y >= self.grid.height {
            return [0.0; 3];
        }
        let at = y * self.grid.width + x;
        add(self.lamps[at], self.suns[at])
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scaled(a: [f32; 3], by: f32) -> [f32; 3] {
    a.map(|value| value * by)
}

fn surfaces(model: &Model) -> Vec<Option<Surface<'_>>> {
    let mut surfaces: Vec<Option<Surface>> = model.light_maps.iter().map(|_| None).collect();
    for polygon in model.polygons() {
        let Some(index) = model.nodes.get(polygon.node).and_then(|node| node.light_map) else { continue };
        let (Some(map), Some(slot)) = (model.light_maps.get(index), surfaces.get_mut(index)) else { continue };
        let Some(node) = model.nodes.get(polygon.node) else { continue };
        let [x, y, z, _] = node.plane;
        let lumels = polygon.positions.iter().map(|&point| map.lumel(point, 1.0)).collect();
        slot.get_or_insert_with(|| Surface { map, normal: Vector::new(x, y, z), polygons: Vec::new() }).polygons.push(lumels);
    }
    surfaces
}

fn direct(scene: &Scene, input: &BakeInput, surface: &Surface) -> Option<Direct> {
    let position_of = lumel_positions(surface.map)?;
    let grid = Grid::new(surface.map, &surface.polygons, SCALE);
    let (corner_x, corner_y) = (surface.map.size[0] as f32, surface.map.size[1] as f32);
    let corners = [position_of(0.0, 0.0), position_of(corner_x, 0.0), position_of(0.0, corner_y), position_of(corner_x, corner_y)];
    let centre = corners.iter().fold(Vector::ZERO, |sum, &corner| sum + corner) / 4.0;
    let reach = corners.iter().map(|&corner| (corner - centre).size()).fold(0.0, f32::max);
    let nearby: Vec<usize> = (0..input.lights.len())
        .filter(|&index| {
            let light = &input.lights[index];
            light.effect == LE_SUNLIGHT || (light.location - centre).size() <= light.world_radius() + reach
        })
        .collect();
    let [side_a, side_b] = axes_of(surface.normal);
    let mut lamps = vec![[0.0; 3]; grid.samples.len()];
    let mut suns = vec![[0.0; 3]; grid.samples.len()];
    let mut shadow_sun = vec![0.0; grid.samples.len()];
    for y in 0..grid.height {
        for x in 0..grid.width {
            let at = y * grid.width + x;
            let Some([u, v]) = grid.samples[at] else { continue };
            let position = position_of(u, v);
            let start = position + surface.normal * LIFT;
            for &index in &nearby {
                let light = &input.lights[index];
                let color = light.color(input.level_brightness);
                let term = scaled(color, 0.5 * light.sample_intensity(position, surface.normal));
                if term.iter().all(|&part| part < SMALLEST_TERM) {
                    continue;
                }
                if light.effect == LE_SUNLIGHT {
                    let to_sun = -light.rotation.vector();
                    let [sun_a, sun_b] = axes_of(to_sun);
                    let clear = (0..SUN_SAMPLES)
                        .filter(|&sample| {
                            let [a, b] = on_disk(jitter(x, y, sample));
                            scene.sun_clear(start, (to_sun + sun_a * (a * SUN_ANGLE) + sun_b * (b * SUN_ANGLE)).safe_normal())
                        })
                        .count();
                    let seen = clear as f32 / SUN_SAMPLES as f32;
                    suns[at] = add(suns[at], scaled(term, seen));
                    if input.shadow_sun == Some(index) {
                        shadow_sun[at] = seen;
                    }
                } else {
                    let clear = (0..LAMP_SAMPLES)
                        .filter(|&sample| {
                            let [a, b] = on_disk(jitter(x, y, sample));
                            !scene.blocked(start, light.location + (side_a * a + side_b * b) * LAMP_RADIUS)
                        })
                        .count();
                    lamps[at] = add(lamps[at], scaled(term, clear as f32 / LAMP_SAMPLES as f32));
                }
            }
        }
    }
    Some(Direct { grid, lamps, suns, shadow_sun })
}

/// At retail's lumel size: bounce and occlusion vary slowly and cost many rays. Each holds bounce RGB and openness.
fn indirect(scene: &Scene, surface: &Surface, direct: &[Option<Direct>]) -> Option<(Grid, Vec<[f32; 4]>)> {
    let position_of = lumel_positions(surface.map)?;
    let grid = Grid::new(surface.map, &surface.polygons, 1);
    let [side_a, side_b] = axes_of(surface.normal);
    let mut gathered = vec![[0.0, 0.0, 0.0, 1.0]; grid.samples.len()];
    for y in 0..grid.height {
        for x in 0..grid.width {
            let at = y * grid.width + x;
            let Some([u, v]) = grid.samples[at] else { continue };
            let start = position_of(u, v) + surface.normal * LIFT;
            let (mut bounce, mut closed) = ([0.0; 3], 0.0);
            for ray in 0..INDIRECT_RAYS {
                let sample = jitter(x, y, ray);
                let [a, b] = on_disk(sample);
                let up = (1.0 - sample[0]).max(0.0).sqrt();
                let along = side_a * a + side_b * b + surface.normal * up;
                let time = match scene.closest(start, start + along * BOUNCE_DISTANCE) {
                    Seen::Open | Seen::Sky => continue,
                    Seen::Other { time } => time,
                    Seen::Surface { time, node, location } => {
                        let lit = scene.model.nodes.get(node).and_then(|node| node.light_map).and_then(|index| {
                            let seen = direct.get(index)?.as_ref()?;
                            Some(seen.light_at(scene.model.light_maps.get(index)?.lumel(location.into(), 1.0)))
                        });
                        bounce = add(bounce, lit.unwrap_or_default());
                        time
                    }
                };
                if time * BOUNCE_DISTANCE < AO_DISTANCE {
                    closed += 1.0;
                }
            }
            let [r, g, b] = scaled(bounce, BOUNCE_ALBEDO / INDIRECT_RAYS as f32);
            gathered[at] = [r, g, b, 1.0 - closed / INDIRECT_RAYS as f32];
        }
    }
    let smooth = smoothed(&grid, &gathered);
    Some((grid, smooth))
}

fn smoothed(grid: &Grid, values: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let mut out = values.to_vec();
    for y in 0..grid.height {
        for x in 0..grid.width {
            let at = y * grid.width + x;
            if grid.samples[at].is_none() {
                continue;
            }
            let (mut sum, mut count) = ([0.0; 4], 0.0);
            for ny in y.saturating_sub(1)..(y + 2).min(grid.height) {
                for nx in x.saturating_sub(1)..(x + 2).min(grid.width) {
                    let near = ny * grid.width + nx;
                    if grid.samples[near].is_some() {
                        for part in 0..4 {
                            sum[part] += values[near][part];
                        }
                        count += 1.0;
                    }
                }
            }
            out[at] = sum.map(|part| part / count);
        }
    }
    out
}

fn filtered(grid: &Grid, values: &[[f32; 4]], u: f32, v: f32) -> [f32; 4] {
    let (fx, fy) = ((u - 0.5).max(0.0), (v - 0.5).max(0.0));
    let (x0, y0) = (fx as usize, fy as usize);
    let (tx, ty) = (fx.fract(), fy.fract());
    let (mut sum, mut total) = ([0.0; 4], 0.0);
    for (x, y, weight) in [(x0, y0, (1.0 - tx) * (1.0 - ty)), (x0 + 1, y0, tx * (1.0 - ty)), (x0, y0 + 1, (1.0 - tx) * ty), (x0 + 1, y0 + 1, tx * ty)] {
        if x < grid.width && y < grid.height && grid.samples[y * grid.width + x].is_some() {
            for part in 0..4 {
                sum[part] += values[y * grid.width + x][part] * weight;
            }
            total += weight;
        }
    }
    if total <= 0.0 { [0.0, 0.0, 0.0, 1.0] } else { sum.map(|part| part / total) }
}

struct Means {
    retail: f64,
    direct: f64,
    last: f64,
    count: usize,
}

fn luma(value: [f32; 3]) -> f64 {
    f64::from(value[0] + value[1] + value[2]) / 3.0
}

fn compose(atlas: &mut Atlas, retail: Option<&(u32, u32, Vec<[u8; 4]>)>, map: &LightMap, ambient: [f32; 3], lit: &Direct, around: &(Grid, Vec<[f32; 4]>), means: &mut Means) {
    let (Ok(left), Ok(top)) = (usize::try_from(map.offset[0]), usize::try_from(map.offset[1])) else { return };
    let (width, height) = (atlas.width as usize, atlas.height as usize);
    for y in 0..lit.grid.height {
        for x in 0..lit.grid.width {
            let at = y * lit.grid.width + x;
            if lit.grid.samples[at].is_none() {
                continue;
            }
            let (u, v) = ((x as f32 + 0.5) / SCALE as f32, (y as f32 + 0.5) / SCALE as f32);
            let [r, g, b, open] = filtered(&around.0, &around.1, u, v);
            let occlusion = AO_DARKEST + (1.0 - AO_DARKEST) * open;
            let lamp_share = 1.0 - LAMP_AO * (1.0 - occlusion);
            let plain = add(scaled(ambient, 0.5), add(lit.lamps[at], lit.suns[at]));
            let light = add(add(scaled(ambient, 0.5 * occlusion), scaled(lit.lamps[at], lamp_share)), add(lit.suns[at], [r, g, b]));
            let (ax, ay) = (left * SCALE + x, top * SCALE + y);
            if ax >= width || ay >= height {
                continue;
            }
            atlas.texels[ay * width + ax] = [light[0], light[1], light[2], lit.shadow_sun[at]].map(p2_math::color_byte);
            means.direct += luma(plain);
            means.last += luma(light);
            means.count += 1;
            if let Some((retail_width, retail_height, texels)) = retail {
                let (rx, ry) = (left + x / SCALE, top + y / SCALE);
                if rx < *retail_width as usize && ry < *retail_height as usize {
                    let [red, green, blue, _] = texels[ry * *retail_width as usize + rx];
                    means.retail += luma([f32::from(red), f32::from(green), f32::from(blue)]) / 255.0;
                }
            }
        }
    }
}

pub fn bake(input: &BakeInput) -> Baked {
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get().saturating_sub(2).max(1));
    let run = || bake_all(input);
    match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(run),
        Err(_) => run(),
    }
}

fn bake_all(input: &BakeInput) -> Baked {
    let started = Instant::now();
    let scene = Scene::new(input.model, input.terrains, input.meshes);
    let surfaces = surfaces(input.model);
    let lit: Vec<Option<Direct>> = surfaces.par_iter().map(|surface| direct(&scene, input, surface.as_ref()?)).collect();
    let around: Vec<_> = surfaces.par_iter().map(|surface| indirect(&scene, surface.as_ref()?, &lit)).collect();
    let retail: Vec<_> = input.model.light_map_textures.iter().map(|atlas| atlas.texture().and_then(|texture| p2_assets::texture_rgba(&texture, None))).collect();
    let mut atlases: Vec<Option<Atlas>> = retail
        .iter()
        .map(|atlas| {
            atlas.as_ref().map(|(width, height, _)| {
                let (width, height) = (*width * SCALE as u32, *height * SCALE as u32);
                Atlas { width, height, texels: vec![[0, 0, 0, 0]; width as usize * height as usize] }
            })
        })
        .collect();
    let mut means = Means { retail: 0.0, direct: 0.0, last: 0.0, count: 0 };
    for (index, map) in input.model.light_maps.iter().enumerate() {
        let (Some(Some(lit)), Some(Some(around))) = (lit.get(index), around.get(index)) else { continue };
        let Some(atlas) = usize::try_from(map.texture).ok() else { continue };
        let Some(Some(target)) = atlases.get_mut(atlas) else { continue };
        let ambient = usize::try_from(map.zone).ok().and_then(|zone| input.zone_ambients.get(zone)).copied().unwrap_or_default();
        compose(target, retail.get(atlas).and_then(Option::as_ref), map, ambient, lit, around, &mut means);
    }
    let count = means.count.max(1) as f64;
    Baked {
        atlases,
        texels: means.count,
        millis: started.elapsed().as_millis(),
        mean_retail: (means.retail / count) as f32,
        mean_direct: (means.direct / count) as f32,
        mean_final: (means.last / count) as f32,
    }
}

#[cfg(test)]
mod tests {
    use p2_assets::{BspNode, BspSurface, LightMapTexture, Vert};
    use p2_math::Rotator;
    use p2_render_cpu::LightEffect;

    use super::*;

    const LUMELS: i32 = 16;
    const ATLAS: i32 = 32;

    fn floor_map() -> LightMap {
        let mut matrix = [0.0; 16];
        matrix[0] = 1.0 / 16.0;
        matrix[5] = 1.0 / 16.0;
        LightMap {
            texture: 0,
            surface: 0,
            zone: 0,
            offset: [0, 0],
            size: [LUMELS, LUMELS],
            world_to_light_map: matrix,
            base: [0.0, 0.0, 0.0],
            axis_x: [256.0, 0.0, 0.0],
            axis_y: [0.0, 256.0, 0.0],
        }
    }

    fn floor() -> Model {
        let surface = BspSurface { material: 0, poly_flags: 0, base: 0, normal: 0, texture_u: 1, texture_v: 2, brush_poly: None, actor: 0, plane: [0.0, 0.0, 1.0, 0.0], light_map_scale: 16.0 };
        let node = BspNode {
            plane: [0.0, 0.0, 1.0, 0.0],
            node_flags: 0,
            vert_pool: 0,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: None,
            render_bound: None,
            zones: [0, 0],
            num_vertices: 4,
            leaves: [-1, -1],
            section: None,
            first_vertex: 0,
            light_map: Some(0),
        };
        Model {
            vectors: vec![[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            points: vec![[0.0, 0.0, 0.0], [256.0, 0.0, 0.0], [256.0, 256.0, 0.0], [0.0, 256.0, 0.0]],
            verts: (0..4).map(|point| Vert { point, side: None }).collect(),
            nodes: vec![node],
            surfaces: vec![surface],
            root_outside: true,
            light_maps: vec![floor_map()],
            light_map_textures: vec![LightMapTexture { light_maps: vec![0], format: 3, width: ATLAS, height: ATLAS, data: [vec![0; (ATLAS * ATLAS / 2) as usize], Vec::new()] }],
            ..Model::default()
        }
    }

    fn roof_over_half() -> MeshCollision {
        let z = 64.0;
        let triangles = [[[0.0, 0.0, z], [128.0, 0.0, z], [128.0, 256.0, z]], [[0.0, 0.0, z], [128.0, 256.0, z], [0.0, 256.0, z]]];
        MeshCollision::placed(triangles, Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)
    }

    fn sun() -> MeshLight {
        MeshLight { effect: LE_SUNLIGHT, rotation: Rotator { pitch: -16384, yaw: 0, roll: 0 }, brightness: 128.0, saturation: 255, ..MeshLight::default() }
    }

    fn texel(atlas: &Atlas, x: usize, y: usize) -> [u8; 4] {
        atlas.texels[y * atlas.width as usize + x]
    }

    #[test]
    fn a_lumel_lies_where_the_renderer_samples_it() {
        let mut matrix = [0.0; 16];
        matrix[0] = 0.5;
        matrix[5] = 0.5;
        matrix[12] = -50.0;
        matrix[13] = -25.0;
        let map = LightMap { world_to_light_map: matrix, base: [100.0, 50.0, 16.0], axis_x: [2.0, 0.0, 0.0], axis_y: [0.0, 2.0, 0.0], ..floor_map() };
        let at = lumel_positions(&map).expect("a lumel")(1.5, 2.5);
        assert!((at - Vector::new(103.0, 55.0, 16.0)).size_squared() < 1e-6);
        assert!(lumel_positions(&LightMap { axis_y: [2.0, 0.0, 0.0], ..map }).is_none());
    }

    #[test]
    fn texels_past_a_polygon_sample_its_edge_and_far_ones_nothing() {
        let polygon = vec![[1.0, 1.0], [3.0, 1.0], [3.0, 3.0], [1.0, 3.0]];
        let grid = Grid::new(&LightMap { size: [4, 4], ..floor_map() }, &[polygon], 1);
        assert_eq!(grid.samples[1 + 4], Some([1.5, 1.5]));
        let [u, v] = grid.samples[4].expect("beside the edge");
        assert!((0.99..1.05).contains(&u) && (1.0..2.0).contains(&v));
        assert!(grid.samples.iter().filter(|sample| sample.is_some()).count() >= 4);
        let far = Grid::new(&LightMap { size: [8, 8], ..floor_map() }, &[vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]], 1);
        assert_eq!(far.samples[7 * 8 + 7], None);
    }

    #[test]
    fn the_world_shades_the_sun_and_alpha_says_so() {
        let (model, roof, lights) = (floor(), [roof_over_half()], [sun()]);
        let baked = bake(&BakeInput { model: &model, terrains: &[], meshes: &roof, lights: &lights, shadow_sun: Some(0), zone_ambients: &[[0.2, 0.2, 0.2]], level_brightness: 1.0 });
        let atlas = baked.atlases[0].as_ref().expect("an atlas");
        assert_eq!((atlas.width, atlas.height), ((ATLAS as usize * SCALE) as u32, (ATLAS as usize * SCALE) as u32));
        let (shade, lit) = (texel(atlas, 4, 16), texel(atlas, 28, 16));
        assert_eq!(shade[3], 0, "shade {shade:?}");
        assert_eq!(lit[3], 255, "lit {lit:?}");
        assert!(lit[0] > shade[0] + 40, "lit {lit:?} shade {shade:?}");
        assert!(shade[0] > 0, "the ambient is gone: {shade:?}");
    }

    #[test]
    fn a_lamp_lights_within_its_radius_and_stops_at_the_world() {
        let lamp = MeshLight { effect: LightEffect(0), location: Vector::new(64.0, 128.0, 32.0), radius: 8.0, brightness: 200.0, saturation: 255, ..MeshLight::default() };
        let (model, roof, lights) = (floor(), [roof_over_half()], [lamp]);
        let baked = bake(&BakeInput { model: &model, terrains: &[], meshes: &roof, lights: &lights, shadow_sun: None, zone_ambients: &[], level_brightness: 1.0 });
        let atlas = baked.atlases[0].as_ref().expect("an atlas");
        let (below, far) = (texel(atlas, 8, 16), texel(atlas, 31, 31));
        assert!(below[0] > 20, "under the lamp {below:?}");
        assert!(far[0] < below[0], "far {far:?}, under the lamp {below:?}");
        assert_eq!(below[3], 0);
    }

    #[test]
    fn a_lone_floor_gets_no_bounce_or_occlusion() {
        let (model, lights) = (floor(), [sun()]);
        let baked = bake(&BakeInput { model: &model, terrains: &[], meshes: &[], lights: &lights, shadow_sun: Some(0), zone_ambients: &[[0.2, 0.2, 0.2]], level_brightness: 1.0 });
        assert!((baked.mean_final - baked.mean_direct).abs() < 1e-3, "final {} direct {}", baked.mean_final, baked.mean_direct);
    }

    fn floor_and_wall() -> Model {
        let mut model = floor();
        let first = model.points.len();
        model.points.extend([[0.0, 0.0, 0.0], [0.0, 256.0, 0.0], [0.0, 256.0, 256.0], [0.0, 0.0, 256.0]]);
        model.verts.extend((first..first + 4).map(|point| Vert { point, side: None }));
        model.vectors.push([1.0, 0.0, 0.0]);
        let mut surface = model.surfaces[0];
        surface.normal = 3;
        surface.plane = [1.0, 0.0, 0.0, 0.0];
        model.surfaces.push(surface);
        let mut node = model.nodes[0];
        node.plane = [1.0, 0.0, 0.0, 0.0];
        node.vert_pool = 4;
        node.surface = 1;
        node.light_map = Some(1);
        model.nodes.push(node);
        model.nodes[0].front = Some(1);
        let mut matrix = [0.0; 16];
        matrix[4] = 1.0 / 16.0;
        matrix[9] = 1.0 / 16.0;
        model.light_maps.push(LightMap { offset: [16, 0], world_to_light_map: matrix, axis_x: [0.0, 256.0, 0.0], axis_y: [0.0, 0.0, 256.0], ..floor_map() });
        model
    }

    #[test]
    fn a_sunlit_floor_bounces_onto_a_wall_and_the_corner_is_occluded() {
        let (model, lights) = (floor_and_wall(), [sun()]);
        let ambient = [[0.2, 0.2, 0.2]];
        let with = |lights: &[MeshLight]| bake(&BakeInput { model: &model, terrains: &[], meshes: &[], lights, shadow_sun: None, zone_ambients: &ambient, level_brightness: 1.0 });
        let (sunlit, dark) = (with(&lights), with(&[]));
        let (sunlit, dark) = (sunlit.atlases[0].as_ref().expect("an atlas"), dark.atlases[0].as_ref().expect("an atlas"));
        let wall_x = 16 * SCALE + 16;
        assert!(texel(sunlit, wall_x, 2)[0] > texel(dark, wall_x, 2)[0] + 10, "with sun {:?}, without {:?}", texel(sunlit, wall_x, 2), texel(dark, wall_x, 2));
        assert!(texel(dark, 0, 16)[0] < texel(dark, 31, 16)[0], "corner {:?}, open floor {:?}", texel(dark, 0, 16), texel(dark, 31, 16));
    }
}
