
use bevy::math::{IVec3, Vec3};
use p2_assets::Model;
use p2_math::Vector;
use p2_render_cpu::MeshLight;
use rustc_hash::FxHashMap;

use crate::level_collision::GREY_ALBEDO;
use crate::modern::linear_light;

/// Tiles across one volume of the atlas. `probes.wesl` holds the same number.
pub(super) const TILES_WIDE: usize = 12;
const SPACING_STEP: f32 = 1.05;
const RAYS: usize = 16;
const FAR: f32 = 200_000.0;
const SURFACE_MARGIN: f32 = 1.0;
/// A bounce texel holds the square root of the light over this. `probes.wesl` holds the same number.
const BOUNCE_RANGE: f32 = 2.0;
const LAMP_CELL: f32 = 1024.0;
/// What an unobstructed floor's read towards the sky sums to with these weights: a read of 1 is open sky.
const OPEN_SKY: f32 = 0.875;
const CELLS_PER_BATCH: usize = 1024;

/// The grid wraps around the world, so the shader needs only its spacing and the atlas size. Each volume of the
/// atlas holds one tile per layer of the grid, a tile the grid's columns with a repeat of the first at each side
/// so filtering across the wrap reads the right neighbour. Half as many layers as columns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ProbeGrid {
    pub(super) columns: i32,
    /// In Unreal units.
    pub(super) smallest_spacing: f32,
    pub(super) most_probes: usize,
}

impl ProbeGrid {
    fn layers(self) -> i32 {
        self.columns / 2
    }

    fn tile(self) -> usize {
        self.columns as usize + 2
    }

    fn volume_width(self) -> usize {
        self.tile() * TILES_WIDE
    }

    pub(super) fn atlas_size(self) -> (usize, usize) {
        (2 * self.volume_width(), self.tile() * (self.layers() as usize).div_ceil(TILES_WIDE))
    }

    fn spacing(self, min: Vec3, max: Vec3) -> f32 {
        let inside = Vec3::new((self.columns - 2) as f32, (self.layers() - 2) as f32, (self.columns - 2) as f32);
        let mut spacing = ((max - min) / inside).max_element().max(self.smallest_spacing);
        while probe_count(min, max, spacing) > self.most_probes {
            spacing *= SPACING_STEP;
        }
        spacing
    }
}

fn span(min: Vec3, max: Vec3, spacing: f32) -> (IVec3, IVec3) {
    ((min / spacing).floor().as_ivec3(), (max / spacing).ceil().as_ivec3())
}

fn probe_count(min: Vec3, max: Vec3, spacing: f32) -> usize {
    let (low, high) = span(min, max, spacing);
    let along = (high - low + IVec3::ONE).as_uvec3();
    along.x as usize * along.y as usize * along.z as usize
}

fn sky_weight(up: f32) -> f32 {
    if up > 0.0 { 0.35 + 0.65 * up } else { 0.2 }
}

struct Bvh {
    tree: p2_collision::TriangleTree,
    triangles: Vec<[Vec3; 3]>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    AtFirst,
    AtNearest,
}

impl Bvh {
    fn new(triangles: &[[[f32; 3]; 3]]) -> Self {
        Self { tree: p2_collision::TriangleTree::new(triangles), triangles: triangles.iter().map(|triangle| triangle.map(Vec3::from)).collect() }
    }

    fn trace(&self, from: Vec3, way: Vec3, far: f32, stop: Stop) -> Option<(f32, u32)> {
        let mut best: Option<(f32, u32)> = None;
        let end = from + way * far;
        self.tree.along(from.to_array().into(), end.to_array().into(), 1.0, |index| {
            if stop == Stop::AtFirst && best.is_some() {
                return -1.0;
            }
            let reach = best.map_or(far, |(distance, _)| distance);
            if let Some(distance) = hit(&self.triangles[index], from, way).filter(|&distance| distance < reach) {
                best = Some((distance, index as u32));
            }
            match (best, stop) {
                (Some(_), Stop::AtFirst) => -1.0,
                (Some((distance, _)), Stop::AtNearest) => distance / far,
                (None, _) => 1.0,
            }
        });
        best
    }

    fn clear(&self, from: Vec3, to: Vec3) -> bool {
        let between = to - from;
        let distance = between.length();
        distance <= SURFACE_MARGIN || self.trace(from, between / distance, distance - SURFACE_MARGIN, Stop::AtFirst).is_none()
    }
}

fn hit(triangle: &[Vec3; 3], from: Vec3, way: Vec3) -> Option<f32> {
    let (edge_a, edge_b) = (triangle[1] - triangle[0], triangle[2] - triangle[0]);
    let cross = way.cross(edge_b);
    let determinant = edge_a.dot(cross);
    if determinant.abs() < 1e-8 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let offset = from - triangle[0];
    let u = offset.dot(cross) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let turned = offset.cross(edge_a);
    let v = way.dot(turned) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(edge_b.dot(turned) * inverse).filter(|&distance| distance > SURFACE_MARGIN)
}

fn ray_ways() -> [Vec3; RAYS] {
    let golden = std::f32::consts::PI * (3.0 - 5.0f32.sqrt());
    std::array::from_fn(|i| {
        let up = 1.0 - (2.0 * i as f32 + 1.0) / RAYS as f32;
        let side = (1.0 - up * up).sqrt();
        let (sin, cos) = (golden * i as f32).sin_cos();
        Vec3::new(side * cos, up, side * sin)
    })
}

pub(super) struct ProbeWorld<'a> {
    pub(super) triangles: &'a [[[f32; 3]; 3]],
    pub(super) albedos: &'a [[f32; 3]],
    /// Triangles from here on are terrain: a point with one above it is in the ground.
    pub(super) terrain_from: usize,
    pub(super) lamps: &'a [MeshLight],
    pub(super) level_brightness: f32,
    pub(super) origin: Vec3,
    pub(super) sun: Option<(Vec3, [f32; 3])>,
    pub(super) model: &'a Model,
    pub(super) zone_ambients: &'a [[f32; 3]],
}

pub(crate) struct ProbeBake {
    pub(super) spacing: f32,
    pub(super) pixels: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
struct Probe {
    open: bool,
    bounce: Vec3,
    sky: f32,
    sky_way: Vec3,
}

struct LampBins {
    bins: FxHashMap<IVec3, Vec<u32>>,
}

impl LampBins {
    fn new(world: &ProbeWorld) -> Self {
        let mut bins: FxHashMap<IVec3, Vec<u32>> = FxHashMap::default();
        for (index, lamp) in world.lamps.iter().enumerate() {
            let center = Vec3::from(p2_bevy_assets::to_bevy(lamp.location)) + world.origin;
            let reach = Vec3::splat(lamp.world_radius());
            let (low, high) = (((center - reach) / LAMP_CELL).floor().as_ivec3(), ((center + reach) / LAMP_CELL).floor().as_ivec3());
            for x in low.x..=high.x {
                for y in low.y..=high.y {
                    for z in low.z..=high.z {
                        bins.entry(IVec3::new(x, y, z)).or_default().push(index as u32);
                    }
                }
            }
        }
        Self { bins }
    }

    fn at(&self, point: Vec3) -> &[u32] {
        self.bins.get(&(point / LAMP_CELL).floor().as_ivec3()).map_or(&[], Vec::as_slice)
    }
}

fn light_at(world: &ProbeWorld, bvh: &Bvh, bins: &LampBins, point: Vec3, normal: Vec3) -> Vec3 {
    let mut light = Vec3::ZERO;
    if let Some((to_sun, color)) = world.sun {
        let facing = normal.dot(to_sun);
        if facing > 0.0 && bvh.trace(point, to_sun, FAR, Stop::AtFirst).is_none() {
            light += Vec3::from(color) * facing;
        }
    }
    // Swapping Bevy's axes back to Unreal's is the same swap.
    let unreal = |at: Vec3| Vector::from(p2_bevy_assets::to_bevy(at.to_array()));
    let (at, facing) = (unreal(point - world.origin), unreal(normal));
    for &index in bins.at(point) {
        let lamp = &world.lamps[index as usize];
        let shown = lamp.sample_intensity(at, facing);
        if shown > 0.0 && bvh.clear(point, Vec3::from(p2_bevy_assets::to_bevy(lamp.location)) + world.origin) {
            // `shown` already holds the doubling that `linear_light` applies.
            light += Vec3::from(linear_light(lamp.color(world.level_brightness))) * (shown * shown * 0.25);
        }
    }
    light
}

fn ambient_at(world: &ProbeWorld, point: Vec3) -> Vec3 {
    let at = Vector::from(p2_bevy_assets::to_bevy((point - world.origin).to_array()));
    let zone = p2_collision::point_zone(world.model, at);
    Vec3::from(world.zone_ambients.get(usize::from(zone)).copied().unwrap_or_default())
}

fn bake_probe(world: &ProbeWorld,bvh: &Bvh, bins: &LampBins, ways: &[Vec3; RAYS], point: Vec3) -> Probe {
    let mut probe = Probe { open: true, ..Probe::default() };
    for &way in ways {
        match bvh.trace(point, way, FAR, Stop::AtNearest) {
            None => {
                let weight = sky_weight(way.y);
                probe.sky += weight;
                probe.sky_way += way * weight;
            }
            Some((distance, index)) => {
                let [a, b, c] = bvh.triangles[index as usize];
                let normal = (b - a).cross(c - a).normalize_or_zero();
                let normal = if normal.dot(way) > 0.0 { -normal } else { normal };
                let at = point + way * distance + normal * SURFACE_MARGIN;
                let albedo = world.albedos.get(index as usize).copied().unwrap_or(GREY_ALBEDO);
                probe.bounce += Vec3::from(albedo) * light_at(world, bvh, bins, at, normal);
            }
        }
    }
    let share = 1.0 / RAYS as f32;
    // The shader shows the bounce under a square root, so a zone ambient `a` in display light joins it as `a * a`.
    let ambient = ambient_at(world, point);
    probe.bounce = probe.bounce * share + ambient * ambient;
    probe.sky *= share / OPEN_SKY;
    probe.sky_way *= 3.0 * share / OPEN_SKY;
    probe
}

fn terrain_above(bvh: &Bvh, terrain_from: usize, point: Vec3) -> bool {
    bvh.trace(point, Vec3::Y, FAR, Stop::AtNearest).is_some_and(|(_, index)| index as usize >= terrain_from)
}

/// A closed probe's filter would read what is past a wall, so it takes its open neighbours' mean.
fn fill_closed(probes: &mut [(IVec3, Probe)]) {
    let places: FxHashMap<IVec3, usize> = probes.iter().enumerate().map(|(index, (cell, _))| (*cell, index)).collect();
    let filled: Vec<(usize, Probe)> = probes
        .iter()
        .enumerate()
        .filter(|(_, (_, probe))| !probe.open)
        .filter_map(|(index, (cell, _))| {
            let near = [IVec3::X, IVec3::NEG_X, IVec3::Y, IVec3::NEG_Y, IVec3::Z, IVec3::NEG_Z].map(|step| places.get(&(*cell + step)).map(|&other| probes[other].1));
            let open: Vec<Probe> = near.into_iter().flatten().filter(|probe| probe.open).collect();
            let share = 1.0 / open.len().max(1) as f32;
            (!open.is_empty()).then(|| {
                let sum = open.iter().fold(Probe::default(), |sum, probe| Probe { open: false, bounce: sum.bounce + probe.bounce, sky: sum.sky + probe.sky, sky_way: sum.sky_way + probe.sky_way });
                (index, Probe { open: false, bounce: sum.bounce * share, sky: sum.sky * share, sky_way: sum.sky_way * share })
            })
        })
        .collect();
    for (index, probe) in filled {
        probes[index].1 = probe;
    }
}

fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn write_probe(pixels: &mut [u8], grid: ProbeGrid, cell: IVec3, probe: &Probe) {
    let (width, _) = grid.atlas_size();
    let layer = cell.y.rem_euclid(grid.layers()) as usize;
    let tile = (layer % TILES_WIDE * grid.tile(), layer / TILES_WIDE * grid.tile());
    let (x, z) = (cell.x.rem_euclid(grid.columns) as usize, cell.z.rem_euclid(grid.columns) as usize);
    let shown = |channel: f32| byte(channel.max(0.0).sqrt() / BOUNCE_RANGE);
    let way = |channel: f32| byte(channel * 0.5 + 0.5);
    let bounce = [shown(probe.bounce.x), shown(probe.bounce.y), shown(probe.bounce.z), 255];
    let sky = [byte(probe.sky), way(probe.sky_way.x), way(probe.sky_way.y), way(probe.sky_way.z)];
    let repeats = |at: usize| [Some(at + 1), (at == 0).then_some(grid.columns as usize + 1)];
    for column in repeats(x).into_iter().flatten() {
        for line in repeats(z).into_iter().flatten() {
            let (px, py) = (tile.0 + column, tile.1 + line);
            let at = |volume: usize| (py * width + volume * grid.volume_width() + px) * 4;
            pixels[at(0)..at(0) + 4].copy_from_slice(&bounce);
            pixels[at(1)..at(1) + 4].copy_from_slice(&sky);
        }
    }
}

fn in_parallel<T: Send>(cells: &[IVec3], each: impl Fn(IVec3) -> T + Sync + Send) -> Vec<T> {
    use rayon::prelude::*;
    crate::lamps::in_bake_pool(|| cells.par_iter().with_min_len(CELLS_PER_BATCH).map(|&cell| each(cell)).collect())
}

pub(super) fn bake_probes(world: &ProbeWorld, grid: ProbeGrid, solid: impl Fn(Vec3) -> bool + Sync + Send) -> Option<ProbeBake> {
    let (mut min, mut max) = (Vec3::MAX, Vec3::MIN);
    for corner in world.triangles.iter().flatten() {
        min = min.min(Vec3::from(*corner));
        max = max.max(Vec3::from(*corner));
    }
    if min.x > max.x {
        return None;
    }
    let spacing = grid.spacing(min, max);
    let (low, high) = span(min, max, spacing);
    let cells: Vec<IVec3> = (low.x..=high.x)
        .flat_map(|x| (low.y..=high.y).flat_map(move |y| (low.z..=high.z).map(move |z| IVec3::new(x, y, z))))
        .collect();
    let bvh = Bvh::new(world.triangles);
    let bins = LampBins::new(world);
    let ways = ray_ways();
    let mut probes = in_parallel(&cells, |cell| {
        let point = cell.as_vec3() * spacing;
        let closed = solid(point) || terrain_above(&bvh, world.terrain_from, point);
        (cell, if closed { Probe::default() } else { bake_probe(world, &bvh, &bins, &ways, point) })
    });
    fill_closed(&mut probes);
    let (width, height) = grid.atlas_size();
    let mut pixels = vec![0; width * height * 4];
    for (cell, probe) in &probes {
        write_probe(&mut pixels, grid, *cell, probe);
    }
    Some(ProbeBake { spacing, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINE: ProbeGrid = ProbeGrid { columns: 256, smallest_spacing: 128.0, most_probes: 1_500_000 };

    fn quad(a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) -> [[[f32; 3]; 3]; 2] {
        [[a, b, c], [a, c, d]]
    }

    fn room(h: f32) -> Vec<[[f32; 3]; 3]> {
        let mut sides = Vec::new();
        sides.extend(quad([-h, -h, -h], [h, -h, -h], [h, -h, h], [-h, -h, h]));
        sides.extend(quad([-h, h, -h], [h, h, -h], [h, h, h], [-h, h, h]));
        sides.extend(quad([-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h]));
        sides.extend(quad([-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h]));
        sides.extend(quad([-h, -h, -h], [-h, -h, h], [-h, h, h], [-h, h, -h]));
        sides.extend(quad([h, -h, -h], [h, -h, h], [h, h, h], [h, h, -h]));
        sides
    }

    static NO_ZONES: std::sync::LazyLock<Model> = std::sync::LazyLock::new(Model::default);

    fn world<'a>(triangles: &'a [[[f32; 3]; 3]], lamps: &'a [MeshLight]) -> ProbeWorld<'a> {
        ProbeWorld { triangles, albedos: &[], terrain_from: triangles.len(), lamps, level_brightness: 1.0, origin: Vec3::ZERO, sun: None, model: &NO_ZONES, zone_ambients: &[] }
    }

    fn zones_split_at_x() -> Model {
        let node = p2_assets::BspNode {
            plane: [1.0, 0.0, 0.0, 0.0],
            node_flags: 0,
            vert_pool: 0,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: None,
            render_bound: None,
            zones: [1, 2],
            num_vertices: 0,
            leaves: [-1, -1],
            section: None,
            first_vertex: 0,
            light_map: None,
        };
        Model { nodes: vec![node], zones: vec![p2_assets::Zone { actor: 0, last_render_time: 0.0 }; 3], root_outside: true, ..Model::default() }
    }

    fn lamp_at(z: f32) -> MeshLight {
        MeshLight { location: Vector::new(0.0, 0.0, z), radius: 20.0, brightness: 255.0, ..MeshLight::default() }
    }

    #[test]
    fn the_tree_finds_the_nearest_triangle_and_any_blocker() {
        let bvh = Bvh::new(&room(100.0));
        let (distance, _) = bvh.trace(Vec3::ZERO, Vec3::Y, FAR, Stop::AtNearest).expect("the roof");
        assert!((distance - 100.0).abs() < 1e-3);
        assert!(!bvh.clear(Vec3::ZERO, Vec3::new(0.0, 500.0, 0.0)));
        assert!(bvh.clear(Vec3::ZERO, Vec3::new(0.0, 50.0, 0.0)));
        assert!(bvh.trace(Vec3::new(0.0, 500.0, 0.0), Vec3::Y, FAR, Stop::AtFirst).is_none());
    }

    #[test]
    fn a_probe_in_a_room_sees_no_sky_and_one_outside_sees_it_above() {
        let triangles = room(100.0);
        let scene = world(&triangles, &[]);
        let (bvh, lamps, ways) = (Bvh::new(&triangles), LampBins::new(&scene), ray_ways());
        assert_eq!(bake_probe(&scene, &bvh, &lamps, &ways, Vec3::ZERO).sky, 0.0);
        let above = bake_probe(&scene, &bvh, &lamps, &ways, Vec3::new(0.0, 500.0, 0.0));
        let (up, down) = (above.sky + above.sky_way.y, above.sky - above.sky_way.y);
        assert!(up > 0.8 && up < 1.3, "{up}");
        assert!(down < up * 0.5, "up {up}, down {down}");
    }

    #[test]
    fn a_lamp_in_a_room_bounces_its_walls_colour_and_one_outside_does_not() {
        let triangles = room(100.0);
        let red = vec![[1.0, 0.0, 0.0]; triangles.len()];
        let inside = [lamp_at(50.0)];
        let scene = ProbeWorld { albedos: &red, ..world(&triangles, &inside) };
        let (bvh, ways) = (Bvh::new(&triangles), ray_ways());
        let bounce = bake_probe(&scene, &bvh, &LampBins::new(&scene), &ways, Vec3::ZERO).bounce;
        assert!(bounce.x > 0.0 && bounce.y == 0.0 && bounce.z == 0.0, "{bounce:?}");
        let outside = [lamp_at(300.0)];
        let scene = world(&triangles, &outside);
        assert_eq!(bake_probe(&scene, &bvh, &LampBins::new(&scene), &ways, Vec3::ZERO).bounce, Vec3::ZERO);
    }

    #[test]
    fn each_probe_stores_the_ambient_of_its_own_zone() {
        let model = zones_split_at_x();
        let (dim, bright) = (Vec3::new(0.2, 0.1, 0.05), Vec3::new(0.5, 0.4, 0.3));
        let ambients = [[0.0; 3], dim.to_array(), bright.to_array()];
        let triangles = room(100.0);
        let scene = ProbeWorld { model: &model, zone_ambients: &ambients, origin: Vec3::new(1000.0, 0.0, 0.0), ..world(&triangles, &[]) };
        let (bvh, lamps, ways) = (Bvh::new(&triangles), LampBins::new(&scene), ray_ways());
        let back = bake_probe(&scene, &bvh, &lamps, &ways, Vec3::new(500.0, 0.0, 0.0)).bounce;
        let front = bake_probe(&scene, &bvh, &lamps, &ways, Vec3::new(1500.0, 0.0, 0.0)).bounce;
        assert_eq!(back, dim * dim);
        assert_eq!(front, bright * bright);
        let (width, height) = FINE.atlas_size();
        let mut pixels = vec![0; width * height * 4];
        write_probe(&mut pixels, FINE, IVec3::ZERO, &Probe { bounce: front, ..Probe::default() });
        let at = (width + 1) * 4;
        assert_eq!(pixels[at..at + 3], bright.to_array().map(|a| byte(a / BOUNCE_RANGE)));
    }

    #[test]
    fn a_closed_probe_takes_only_its_open_neighbours() {
        let open = Probe { open: true, sky: 1.0, bounce: Vec3::ONE, ..Probe::default() };
        let half = Probe { open: true, sky: 0.5, ..Probe::default() };
        let mut probes = vec![(IVec3::ZERO, Probe::default()), (IVec3::X, open), (IVec3::Y, half), (IVec3::NEG_X, Probe::default()), (IVec3::new(5, 5, 5), Probe::default())];
        fill_closed(&mut probes);
        assert_eq!((probes[0].1.sky, probes[0].1.bounce), (0.75, Vec3::splat(0.5)));
        assert!(!probes[0].1.open);
        assert_eq!(probes[3].1.sky, 0.0);
        assert_eq!(probes[4].1.sky, 0.0);
    }

    #[test]
    fn the_grid_wraps_and_repeats_its_first_probes() {
        let (width, height) = FINE.atlas_size();
        let mut pixels = vec![0; width * height * 4];
        let probe = Probe { bounce: Vec3::ONE, ..Probe::default() };
        write_probe(&mut pixels, FINE, IVec3::new(-FINE.columns, FINE.layers() + TILES_WIDE as i32 + 1, 0), &probe);
        let at = |x: usize, y: usize, volume: usize| pixels[(y * width + volume * FINE.volume_width() + x) * 4];
        let tile = FINE.tile();
        let last = FINE.columns as usize + 1;
        assert_ne!(at(tile + 1, tile + 1, 0), 0);
        assert_ne!(at(tile + last, tile + 1, 0), 0);
        assert_ne!(at(tile + 1, tile + last, 0), 0);
        assert_eq!(at(tile + 2, tile + 1, 0), 0);
        assert_eq!(at(tile + 1, tile + 1, 1), 0);
        assert_eq!(pixels[((tile + 1) * width + tile + 1) * 4 + 3], 255);
    }

    #[test]
    fn a_large_map_widens_its_spacing_to_stay_in_budget_and_in_the_grid() {
        let (min, max) = (Vec3::new(-90_000.0, 0.0, -90_000.0), Vec3::new(90_000.0, 9_000.0, 90_000.0));
        let spacing = FINE.spacing(min, max);
        assert!(probe_count(min, max, spacing) <= FINE.most_probes);
        let (low, high) = span(min, max, spacing);
        assert!(high.x - low.x < FINE.columns && high.y - low.y < FINE.layers());
        assert_eq!(FINE.spacing(Vec3::ZERO, Vec3::splat(10.0)), FINE.smallest_spacing);
    }

    #[test]
    fn a_bake_fills_the_whole_atlas() {
        let triangles = room(100.0);
        let scene = world(&triangles, &[]);
        let open = bake_probes(&scene, FINE, |_| false).expect("a bake");
        let half_closed = bake_probes(&scene, FINE, |point| point.y > 0.0).expect("a bake");
        let (width, height) = FINE.atlas_size();
        assert_eq!(open.pixels.len(), width * height * 4);
        assert_eq!(open.spacing, half_closed.spacing);
        assert!(bake_probes(&world(&[], &[]), FINE, |_| false).is_none());
    }
}
