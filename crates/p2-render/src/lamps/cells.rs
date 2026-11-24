//! Which lamps light each cell of a grid over the map, so a pixel walks the lamps of its cell and no others. A lamp's
//! reach is read off its shadow tile, redrawn here on the CPU texel for texel as the atlas pass draws it: a cell loses
//! a lamp only where the atlas shades every point of it. Masked casters are left out, which only lists more.

use bevy::math::Vec3;

use super::atlas::{FACE_AXES, GUARD_TEXELS, NEAR};

const MOST_CELLS: u64 = 1 << 22;
/// The shader takes a cell's place apart from 10 bits an axis.
const MOST_CELLS_AN_AXIS: u32 = 1023;
/// `lamp_shadow.wesl` moves the point it looks up by 1.5 texels along the normal, lets it be up to 3 texels nearer and its
/// edge's reads 1.5 more, and reads at places up to 1 texel from it, each with the texel past it. It also lets the point
/// be nearer by a share of its distance, for the steps of a 16 bit float.
const LOOKUP_OFFSET_TEXELS: f32 = 1.5;
const LOOKUP_MARGIN_TEXELS: f32 = 4.5;
const LOOKUP_REACH_TEXELS: f32 = 1.0;
const DEPTH_STEP: f32 = 0.001;
/// `lamp_shadow.wesl` skips a lamp that gives a pixel less display light than this, and weighs colours so.
const LAMP_SMALLEST: f32 = 0.01;
const LUMINANCE: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Rounding here and in the shader differs: a lamp is dropped only this far under what the shader skips.
const DIM_SLACK: f32 = 0.9;
/// Covers the rounding between a pixel's cell in the shader and here.
const CELL_SLACK: f32 = 0.5;

pub(super) struct CellLamp<'a> {
    pub position: [f32; 3],
    pub radius: f32,
    /// The side of one face of its shadow tile in texels. A lamp without a tile lights all it reaches.
    pub face: Option<u32>,
    /// Corners of its casters in the vertices, three a triangle.
    pub casters: &'a [u32],
    /// Its display light at its own place, weighed as the shader weighs it.
    pub strength: f32,
    /// A spot's direction, and the scale and offset that turn the cosine of the angle off it into its share of light.
    pub spot: Option<([f32; 3], f32, f32)>,
}

impl<'a> CellLamp<'a> {
    /// From the lamp's texels in the book, so a cell is judged by the numbers the shader reads.
    pub(super) fn of(record: &[[f32; 4]; RECORD_TEXELS], face: Option<u32>, casters: &'a [u32]) -> Self {
        let [[x, y, z, radius], [r, g, b, _], [along_x, along_y, scale, offset], ..] = *record;
        let strength = [r, g, b].iter().zip(LUMINANCE).map(|(channel, weight)| channel.max(0.0).sqrt() * weight).sum();
        let along = [along_x, along_y, (1.0 - along_x * along_x - along_y * along_y).max(0.0).sqrt().copysign(scale)];
        Self { position: [x, y, z], radius: radius.abs(), face, casters, strength, spot: (radius < 0.0).then_some((along, scale.abs(), offset)) }
    }

    /// The most display light the lamp gives any point of a box, as the shader counts it before shadows. The box is seen
    /// from the lamp and `nearest` is its distance.
    fn most_light(&self, low: [f32; 3], high: [f32; 3], nearest: f32) -> f32 {
        let x = (nearest / self.radius).min(1.0);
        let falloff = 1.0 - x * x * (3.0 - 2.0 * x);
        let cone = self.spot.map_or(1.0, |(along, scale, offset)| {
            let (low, high) = (Vec3::from(low), Vec3::from(high));
            let middle = 0.5 * (low + high);
            let (far, around) = (middle.length(), 0.5 * (high - low).length());
            if far <= around {
                return 1.0;
            }
            let off = (Vec3::from(along).dot(middle) / far).clamp(-1.0, 1.0).acos() - (around / far).asin();
            let share = (off.max(0.0).cos() * scale + offset).clamp(0.0, 1.0);
            share * share
        });
        self.strength * falloff * cone
    }
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct LampCells {
    pub origin: [f32; 3],
    pub cell: f32,
    pub dims: [u32; 3],
    /// A cell's lamps are `lamps[starts[cell]..starts[cell + 1]]`.
    pub starts: Vec<u32>,
    pub lamps: Vec<u32>,
    /// How many pairs of a lamp and a cell within its radius there are, lit or not.
    pub in_reach: u64,
    /// For each lamp, how far the casters it sees through each face of its tile lie.
    pub reach: Vec<[f32; 6]>,
}

impl LampCells {
    pub(super) fn of(&self, cell: usize) -> &[u32] {
        &self.lamps[self.starts[cell] as usize..self.starts[cell + 1] as usize]
    }

    fn index(&self, [x, y, z]: [u32; 3]) -> usize {
        ((z * self.dims[1] + y) * self.dims[0] + x) as usize
    }
}

fn grid(lamps: &[CellLamp], smallest_cell: f32) -> LampCells {
    let (mut low, mut high) = ([f32::MAX; 3], [f32::MIN; 3]);
    for lamp in lamps {
        for axis in 0..3 {
            low[axis] = low[axis].min(lamp.position[axis] - lamp.radius);
            high[axis] = high[axis].max(lamp.position[axis] + lamp.radius);
        }
    }
    let mut cell = smallest_cell;
    loop {
        let dims: [u32; 3] = std::array::from_fn(|axis| ((high[axis] - low[axis]) / cell).ceil().max(1.0) as u32);
        if dims.iter().all(|&side| side <= MOST_CELLS_AN_AXIS) && dims.iter().map(|&side| u64::from(side)).product::<u64>() <= MOST_CELLS {
            return LampCells { origin: low, cell, dims, ..LampCells::default() };
        }
        cell *= 2.0;
    }
}

/// A lamp's shadow tile: for each face the distance to the nearest caster over the radius, 1 where open, then the same
/// at half the side over and over, each texel the farthest of its four.
struct Cube {
    side: usize,
    levels: Vec<Vec<f32>>,
}

impl Cube {
    fn drawn(side: usize, lamp: &CellLamp, vertices: &[[f32; 3]]) -> Self {
        let mut depths = vec![1.0; 6 * side * side];
        for corners in lamp.casters.as_chunks::<3>().0 {
            let triangle: [[f32; 3]; 3] = std::array::from_fn(|corner| std::array::from_fn(|axis| vertices[corners[corner] as usize][axis] - lamp.position[axis]));
            for (face, axes) in FACE_AXES.iter().enumerate() {
                draw_triangle(&mut depths[face * side * side..(face + 1) * side * side], side, triangle.map(|point| axes.map(|axis| Vec3::from(point).dot(Vec3::from(axis)))), lamp.radius);
            }
        }
        let mut levels = vec![depths];
        let mut size = side;
        while size > 1 {
            let (half, finer) = (size / 2, &levels[levels.len() - 1]);
            let mut coarser = vec![0.0f32; 6 * half * half];
            for face in 0..6 {
                for y in 0..size {
                    for x in 0..size {
                        let into = &mut coarser[face * half * half + (y / 2).min(half - 1) * half + (x / 2).min(half - 1)];
                        *into = into.max(finer[face * size * size + y * size + x]);
                    }
                }
            }
            levels.push(coarser);
            size = half;
        }
        Self { side, levels }
    }

    /// Over the radius, for each face: nine in ten of its texels that hold a caster hold one no farther.
    fn reach(&self) -> [f32; 6] {
        let texels = self.side * self.side;
        let mut held = Vec::new();
        std::array::from_fn(|face| {
            held.clear();
            held.extend(self.levels[0][face * texels..(face + 1) * texels].iter().copied().filter(|&depth| depth < 1.0));
            let at = held.len() * 9 / 10;
            if held.is_empty() { 0.0 } else { *held.select_nth_unstable_by(at, f32::total_cmp).1 }
        })
    }

    /// Whether any texel a lookup inside the box could read holds `depth` or more. The box is seen from the lamp.
    fn open_to(&self, low: [f32; 3], high: [f32; 3], depth: f32) -> bool {
        let half = 0.5 * self.side as f32;
        let span = half - GUARD_TEXELS;
        FACE_AXES.iter().enumerate().any(|(face, axes)| {
            let along = |axis: [f32; 3]| {
                let ends = [0, 1, 2].map(|at| [axis[at] * low[at], axis[at] * high[at]]);
                (ends.iter().map(|[a, b]| a.min(*b)).sum::<f32>(), ends.iter().map(|[a, b]| a.max(*b)).sum::<f32>())
            };
            let (near, far) = along(axes[2]);
            if far <= 1e-3 {
                return false;
            }
            let near = near.max(1e-3);
            let slopes = |(from, to): (f32, f32)| (from / if from < 0.0 { near } else { far }, to / if to > 0.0 { near } else { far });
            let (left, right) = slopes(along(axes[0]));
            let (bottom, top) = slopes(along(axes[1]));
            if left > 1.0 || right < -1.0 || bottom > 1.0 || top < -1.0 {
                return false;
            }
            let last = self.side as i32 - 1;
            let taps = |from: f32, to: f32| {
                let texel = |slope: f32, by: f32| (half + slope.clamp(-1.0, 1.0) * span + by - 0.5).floor() as i32;
                (texel(from, -LOOKUP_REACH_TEXELS).clamp(0, last), (texel(to, LOOKUP_REACH_TEXELS) + 1).clamp(0, last))
            };
            let (x0, x1) = taps(left, right);
            let (y0, y1) = taps(-top, -bottom);
            let mut level = 0;
            while ((x1 - x0).max(y1 - y0) >> level) >= 16 && level + 1 < self.levels.len() {
                level += 1;
            }
            let size = (self.side >> level).max(1);
            let texels = &self.levels[level][face * size * size..(face + 1) * size * size];
            let most = size - 1;
            let at = |texel: i32| (texel as usize >> level).min(most);
            (at(y0)..=at(y1)).any(|y| texels[y * size + at(x0)..=y * size + at(x1)].iter().any(|&texel| texel >= depth))
        })
    }
}

/// `corners` in the face's own axes: x right, y up, z out of the lamp.
fn draw_triangle(depths: &mut [f32], side: usize, corners: [[f32; 3]; 3], radius: f32) {
    let half = 0.5 * side as f32;
    let span = half - GUARD_TEXELS;
    let wide = half / span;
    if corners.iter().all(|c| c[2] < NEAR)
        || corners.iter().all(|c| c[0] > c[2] * wide)
        || corners.iter().all(|c| -c[0] > c[2] * wide)
        || corners.iter().all(|c| c[1] > c[2] * wide)
        || corners.iter().all(|c| -c[1] > c[2] * wide)
    {
        return;
    }
    let mut seen = [[0.0f32; 2]; 4];
    let mut count = 0;
    for index in 0..3 {
        let (from, to) = (corners[index], corners[(index + 1) % 3]);
        let mut push = |point: [f32; 3]| {
            seen[count] = [half + point[0] / point[2] * span, half - point[1] / point[2] * span];
            count += 1;
        };
        if from[2] >= NEAR {
            push(from);
        }
        if (from[2] >= NEAR) != (to[2] >= NEAR) {
            let share = (NEAR - from[2]) / (to[2] - from[2]);
            push([from[0] + (to[0] - from[0]) * share, from[1] + (to[1] - from[1]) * share, NEAR]);
        }
    }
    let seen = &seen[..count];
    if count < 3 {
        return;
    }
    let bounds = |axis: usize| {
        let (low, high) = seen.iter().fold((f32::MAX, f32::MIN), |(low, high), point| (low.min(point[axis]), high.max(point[axis])));
        (((low - 0.5).ceil() as i32).max(0), ((high - 0.5).floor() as i32).min(side as i32 - 1))
    };
    let ((x0, x1), (y0, y1)) = (bounds(0), bounds(1));
    if x0 > x1 || y0 > y1 {
        return;
    }
    let [a, b, c] = corners.map(Vec3::from);
    let normal = (b - a).cross(c - a);
    let plane = normal.dot(a);
    let along = |index: usize| [seen[(index + 1) % count][0] - seen[index][0], seen[(index + 1) % count][1] - seen[index][1]];
    let winding = (0..count).map(|index| (seen[index], along(index))).map(|(from, along)| from[0] * along[1] - from[1] * along[0]).sum::<f32>().signum();
    // A texel on an edge two triangles share belongs to one of them: they run along it in opposite directions.
    let edges: [([f32; 2], [f32; 2], bool); 4] = std::array::from_fn(|index| {
        let (from, along) = (seen[index % count], along(index % count));
        let (across, down) = (winding * along[0], winding * along[1]);
        (from, along, down > 0.0 || down == 0.0 && across > 0.0)
    });
    let edges = &edges[..count];
    for y in y0..=y1 {
        let py = y as f32 + 0.5;
        let rise = (half - py) / span;
        for x in x0..=x1 {
            let px = x as f32 + 0.5;
            let covered = edges.iter().all(|&(from, along, owns_edge)| {
                let turn = winding * (along[0] * (py - from[1]) - along[1] * (px - from[0]));
                turn > 0.0 || turn == 0.0 && owns_edge
            });
            if !covered {
                continue;
            }
            let ray = Vec3::new((px - half) / span, rise, 1.0);
            let slope = normal.dot(ray);
            if slope.abs() < 1e-12 {
                continue;
            }
            let depth = plane / slope * ray.length() / radius;
            let texel = &mut depths[y as usize * side + x as usize];
            *texel = texel.min(depth);
        }
    }
}

enum Reach {
    Out,
    Dark,
    Lit,
}

/// Whether the lamp's light gets into a box of this side whose low corner, seen from the lamp, is `corner`.
fn box_reach(lamp: &CellLamp, cube: Option<&Cube>, corner: [f32; 3], side: f32) -> Reach {
    let low = corner.map(|at| at - CELL_SLACK);
    let high = low.map(|at| at + side + 2.0 * CELL_SLACK);
    let nearest = |grown: f32| (0..3).map(|axis| (low[axis] - grown).max(-high[axis] - grown).max(0.0)).map(|gap| gap * gap).sum::<f32>().sqrt();
    if nearest(0.0) >= lamp.radius {
        return Reach::Out;
    }
    if lamp.most_light(low, high, nearest(0.0)) < DIM_SLACK * LAMP_SMALLEST {
        return Reach::Dark;
    }
    let lights = cube.zip(lamp.face).is_none_or(|(cube, face)| {
        let farthest = (0..3).map(|axis| low[axis].abs().max(high[axis].abs())).map(|reach| reach * reach).sum::<f32>().sqrt().min(lamp.radius);
        let texel = 2.0 * farthest / face as f32;
        let grown = texel * LOOKUP_OFFSET_TEXELS;
        let depth = (nearest(grown) - texel * LOOKUP_MARGIN_TEXELS - farthest * DEPTH_STEP) / lamp.radius;
        depth <= 0.0 || cube.open_to(low.map(|at| at - grown), high.map(|at| at + grown), depth)
    });
    if lights { Reach::Lit } else { Reach::Dark }
}

/// The cells of `grid` a lamp lights, and how many lie within its radius. Cells are tried eight at a time as one box
/// first: a box the lamp leaves dark holds no lit cell.
fn lit_cells(grid: &LampCells, lamp: &CellLamp, vertices: &[[f32; 3]]) -> (Vec<u32>, u64, [f32; 6]) {
    let cube = lamp.face.map(|face| Cube::drawn(face as usize, lamp, vertices));
    let blocks = |axis: usize| {
        let cell = |at: f32| (((at - grid.origin[axis]) / grid.cell).floor().max(0.0) as u32).min(grid.dims[axis] - 1);
        cell(lamp.position[axis] - lamp.radius) / 2..=cell(lamp.position[axis] + lamp.radius) / 2
    };
    let corner = |cell: [u32; 3]| -> [f32; 3] { std::array::from_fn(|axis| grid.origin[axis] + cell[axis] as f32 * grid.cell - lamp.position[axis]) };
    let (mut lit, mut in_reach) = (Vec::new(), 0);
    for z in blocks(2) {
        for y in blocks(1) {
            for x in blocks(0) {
                let block = [2 * x, 2 * y, 2 * z];
                let whole = box_reach(lamp, cube.as_ref(), corner(block), 2.0 * grid.cell);
                if matches!(whole, Reach::Out) {
                    continue;
                }
                for child in 0..8u32 {
                    let cell: [u32; 3] = std::array::from_fn(|axis| block[axis] + (child >> axis & 1));
                    if (0..3).any(|axis| cell[axis] >= grid.dims[axis]) {
                        continue;
                    }
                    let reach = box_reach(lamp, cube.as_ref().filter(|_| matches!(whole, Reach::Lit)), corner(cell), grid.cell);
                    if matches!(reach, Reach::Out) {
                        continue;
                    }
                    in_reach += 1;
                    if matches!(whole, Reach::Lit) && matches!(reach, Reach::Lit) {
                        lit.push(grid.index(cell) as u32);
                    }
                }
            }
        }
    }
    (lit, in_reach, cube.as_ref().map_or([1.0; 6], Cube::reach).map(|share| share * lamp.radius))
}

/// Cells no smaller than `smallest_cell`, grown until the grid over every lamp's reach is small enough to hold.
pub(super) fn lamp_cells(vertices: &[[f32; 3]], lamps: &[CellLamp], smallest_cell: f32, threads: usize) -> LampCells {
    if lamps.is_empty() {
        return LampCells::default();
    }
    let mut grid = grid(lamps, smallest_cell);
    let lit = super::on_threads(lamps.len(), threads, |lamp| lit_cells(&grid, &lamps[lamp], vertices));
    let cells = grid.dims.iter().product::<u32>() as usize;
    let mut starts = vec![0u32; cells + 1];
    for &cell in lit.iter().flat_map(|(cells, ..)| cells) {
        starts[cell as usize + 1] += 1;
    }
    for cell in 0..cells {
        starts[cell + 1] += starts[cell];
    }
    let mut filled = starts.clone();
    let mut listed = vec![0u32; starts[cells] as usize];
    for (lamp, (cells, ..)) in lit.iter().enumerate() {
        for &cell in cells {
            listed[filled[cell as usize] as usize] = lamp as u32;
            filled[cell as usize] += 1;
        }
    }
    grid.in_reach = lit.iter().map(|(_, in_reach, _)| in_reach).sum();
    grid.reach = lit.iter().map(|&(_, _, reach)| reach).collect();
    grid.starts = starts;
    grid.lamps = listed;
    grid
}

/// Texels of a lamp's record in the book. `lamp_shadow.wesl` holds the same number.
pub(super) const RECORD_TEXELS: usize = 7;
pub(super) const HEADER_TEXELS: usize = 2;
/// The book's width in texels. `lamp_shadow.wesl` holds the same number.
pub(super) const BOOK_WIDTH: usize = 2048;
/// The rows the book ends with, counted back from its end: the movers' tile grid starts this far back, then their tiles'
/// row, then the capsules' fine and coarse rows. `lamp_shadow.wesl` holds the same numbers.
pub(super) const TILE_GRID_FROM_END: u32 = 8;
pub(super) const TILES_FROM_END: u32 = 3;
pub(super) const CAPSULES_FROM_END: u32 = 2;
/// A texel's number goes through a float, exact up to 2^24; the movers' rows less, so the whole book is no taller than
/// the 8192 texels every GPU takes.
const MOST_BOOK_TEXELS: usize = (1 << 24) - TILE_GRID_FROM_END as usize * BOOK_WIDTH;

/// What `lamp_shadow.wesl` reads, four floats a texel: a header (the grid's corner and its cell; its sides and where the
/// cells start), every lamp's record, a texel for each cell (where its list starts and how many lamps it holds) and
/// the lists, four lamps a texel. Lists past the largest book are dropped, cells farthest along first.
pub(super) fn book_texels(cells: &LampCells, records: &[[[f32; 4]; RECORD_TEXELS]]) -> Vec<[f32; 4]> {
    let cells_start = HEADER_TEXELS + records.len() * RECORD_TEXELS;
    let cell_count = cells.starts.len().saturating_sub(1);
    let [x, y, z] = cells.origin;
    let mut texels = vec![[x, y, z, cells.cell], [cells.dims[0] as f32, cells.dims[1] as f32, cells.dims[2] as f32, cells_start as f32]];
    texels.extend(records.iter().flatten());
    texels.resize(cells_start + cell_count, [0.0; 4]);
    for cell in 0..cell_count {
        let lamps = cells.of(cell);
        if lamps.is_empty() || texels.len() + lamps.len().div_ceil(4) > MOST_BOOK_TEXELS {
            continue;
        }
        texels[cells_start + cell] = [texels.len() as f32, lamps.len() as f32, 0.0, 0.0];
        texels.extend(lamps.chunks(4).map(|four| std::array::from_fn(|at| four.get(at).map_or(0.0, |&lamp| lamp as f32))));
    }
    texels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_shader_reads_the_book_as_it_is_written() {
        let shader = include_str!("../lamp_shadow.wesl");
        let pairs = [
            ("BOOK_WIDTH", BOOK_WIDTH as u32),
            ("BOOK_HEADER", HEADER_TEXELS as u32),
            ("BOOK_RECORD", RECORD_TEXELS as u32),
            ("TILE_GRID_FROM_END", TILE_GRID_FROM_END),
            ("TILES_FROM_END", TILES_FROM_END),
            ("CAPSULES_FROM_END", CAPSULES_FROM_END),
        ];
        for (name, value) in pairs {
            assert_eq!(shader_const(shader, name), f64::from(value), "{name}");
        }
    }

    fn wall(at: f32) -> Vec<[f32; 3]> {
        vec![[at, -4000.0, -4000.0], [at, 4000.0, -4000.0], [at, 4000.0, 4000.0], [at, -4000.0, -4000.0], [at, 4000.0, 4000.0], [at, -4000.0, 4000.0]]
    }

    fn cells_of(vertices: &[[f32; 3]], face: Option<u32>) -> LampCells {
        let casters: Vec<u32> = (0..vertices.len() as u32).collect();
        lamp_cells(vertices, &[CellLamp { position: [0.0; 3], radius: 1000.0, face, casters: &casters, strength: 1.0, spot: None }], 128.0, 2)
    }

    fn cell_at(cells: &LampCells, point: [f32; 3]) -> usize {
        cells.index(std::array::from_fn(|axis| ((point[axis] - cells.origin[axis]) / cells.cell) as u32))
    }

    #[test]
    fn a_lamp_in_the_open_is_listed_in_every_cell_it_reaches_and_no_other() {
        let cells = cells_of(&[], Some(64));
        assert_eq!(cells.dims, [16, 16, 16]);
        assert_eq!(cells.of(cell_at(&cells, [10.0, 10.0, 10.0])), [0]);
        assert_eq!(cells.of(cell_at(&cells, [900.0, 10.0, 10.0])), [0]);
        assert!(cells.of(cell_at(&cells, [900.0, 900.0, 900.0])).is_empty());
    }

    #[test]
    fn a_wall_takes_the_lamp_off_the_cells_behind_it_and_keeps_those_before_and_across_it() {
        let cells = cells_of(&wall(300.0), Some(64));
        assert_eq!(cells.of(cell_at(&cells, [200.0, 10.0, 10.0])), [0]);
        assert_eq!(cells.of(cell_at(&cells, [300.0, 10.0, 10.0])), [0]);
        assert_eq!(cells.of(cell_at(&cells, [-900.0, 10.0, 10.0])), [0]);
        for behind in [[600.0, 10.0, 10.0], [900.0, 10.0, 10.0], [800.0, 300.0, -300.0]] {
            assert!(cells.of(cell_at(&cells, behind)).is_empty(), "{behind:?}");
        }
    }

    #[test]
    fn a_doorway_lets_the_lamp_into_the_cells_it_shines_on_through_it() {
        let mut vertices = Vec::new();
        for (y0, y1, z0, z1) in [(-4000.0, -100.0, -4000.0, 4000.0), (100.0, 4000.0, -4000.0, 4000.0), (-100.0, 100.0, 100.0, 4000.0), (-100.0, 100.0, -4000.0, -100.0)] {
            vertices.extend([[300.0, y0, z0], [300.0, y1, z0], [300.0, y1, z1], [300.0, y0, z0], [300.0, y1, z1], [300.0, y0, z1]]);
        }
        let cells = cells_of(&vertices, Some(64));
        assert_eq!(cells.of(cell_at(&cells, [800.0, 10.0, 10.0])), [0]);
        assert!(cells.of(cell_at(&cells, [800.0, 600.0, 10.0])).is_empty());
    }

    #[test]
    fn cells_tried_eight_at_a_time_come_out_as_each_tried_alone() {
        let vertices = wall(300.0);
        let casters: Vec<u32> = (0..vertices.len() as u32).collect();
        let lamp = CellLamp { position: [37.0, -11.0, 5.0], radius: 1000.0, face: Some(64), casters: &casters, strength: 1.0, spot: None };
        let cells = lamp_cells(&vertices, std::slice::from_ref(&lamp), 128.0, 1);
        let cube = Cube::drawn(64, &lamp, &vertices);
        let mut alone = Vec::new();
        for z in 0..cells.dims[2] {
            for y in 0..cells.dims[1] {
                for x in 0..cells.dims[0] {
                    let corner = std::array::from_fn(|axis| cells.origin[axis] + [x, y, z][axis] as f32 * cells.cell - lamp.position[axis]);
                    if matches!(box_reach(&lamp, Some(&cube), corner, cells.cell), Reach::Lit) {
                        alone.push(cells.index([x, y, z]));
                    }
                }
            }
        }
        let listed: Vec<usize> = (0..cells.starts.len() - 1).filter(|&cell| !cells.of(cell).is_empty()).collect();
        assert!(alone.len() > 100 && alone.len() < cells.starts.len() - 100, "{}", alone.len());
        assert_eq!(listed, alone);
    }

    fn shader_share(record: &[[f32; 4]; RECORD_TEXELS], point: Vec3) -> f32 {
        let [[x, y, z, radius], [r, g, b, _], [along_x, along_y, scale, offset], ..] = *record;
        let to_light = Vec3::new(x, y, z) - point;
        let x2 = to_light.length_squared() / (radius * radius);
        if x2 >= 1.0 {
            return 0.0;
        }
        let mut cone = 1.0;
        if radius < 0.0 {
            let along = Vec3::new(along_x, along_y, (1.0 - along_x * along_x - along_y * along_y).max(0.0).sqrt() * scale.signum());
            let share = ((-along).dot(to_light / to_light.length().max(0.01)) * scale.abs() + offset).clamp(0.0, 1.0);
            cone = share * share;
        }
        let shown = Vec3::new(r, g, b).map(f32::sqrt) * ((1.0 - x2 * (3.0 - 2.0 * x2.sqrt())) * cone);
        shown.dot(Vec3::from(LUMINANCE))
    }

    fn spot_record(along: Vec3, sine: f32, color: f32) -> [[f32; 4]; RECORD_TEXELS] {
        let scale = 1.0 / (1.0 - sine);
        [[30.0, -20.0, 10.0, -1000.0], [color; 4], [along.x, along.y, scale.copysign(along.z), -sine * scale], [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]]
    }

    #[test]
    fn a_cell_loses_only_a_lamp_the_shader_would_skip_at_every_point_of_it() {
        let point_lamp = [[30.0, -20.0, 10.0, 1000.0], [0.05; 4], [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]];
        let down = spot_record(Vec3::new(0.2, -0.9, -0.38).normalize(), 0.5, 0.6);
        let narrow = spot_record(Vec3::new(-0.7, 0.1, 0.7).normalize(), 0.875, 1.0);
        for (record, most_listed) in [(point_lamp, 1.0), (down, 0.45), (narrow, 0.2)] {
            let lamp = CellLamp::of(&record, None, &[]);
            let cells = lamp_cells(&[], std::slice::from_ref(&lamp), 128.0, 2);
            let (mut listed, mut dropped) = (0, 0);
            for index in 0..cells.starts.len() - 1 {
                let at = [index as u32 % cells.dims[0], index as u32 / cells.dims[0] % cells.dims[1], index as u32 / (cells.dims[0] * cells.dims[1])];
                let corner = Vec3::from(std::array::from_fn(|axis| cells.origin[axis] + at[axis] as f32 * cells.cell));
                if !cells.of(index).is_empty() {
                    listed += 1;
                    continue;
                }
                dropped += 1;
                for step in 0..125u32 {
                    let inside = Vec3::new((step % 5) as f32, (step / 5 % 5) as f32, (step / 25) as f32) * (cells.cell / 4.0);
                    let share = shader_share(&record, corner + inside);
                    assert!(share < LAMP_SMALLEST, "cell {at:?} lost a lamp that gives {share} at {inside}");
                }
            }
            let share = listed as f32 / (listed + dropped) as f32;
            assert!(listed > 20 && share <= most_listed, "{listed} of {} cells listed", listed + dropped);
        }
    }

    #[test]
    fn a_lamp_without_a_tile_lights_through_walls() {
        let cells = cells_of(&wall(300.0), None);
        assert_eq!(cells.of(cell_at(&cells, [900.0, 10.0, 10.0])), [0]);
    }

    #[test]
    fn the_cube_holds_the_distance_to_a_wall_over_the_radius() {
        let vertices = wall(300.0);
        let casters: Vec<u32> = (0..6).collect();
        let cube = Cube::drawn(64, &CellLamp { position: [0.0; 3], radius: 1000.0, face: Some(64), casters: &casters, strength: 1.0, spot: None }, &vertices);
        let middle = cube.levels[0][32 * 64 + 32];
        assert!((middle - 0.3).abs() < 0.01, "{middle}");
        assert!(cube.levels[0][64 * 64..2 * 64 * 64].iter().all(|&texel| texel == 1.0));
        assert_eq!(cube.levels.len(), 7);
    }

    #[test]
    fn a_face_reaches_as_far_as_the_casters_it_sees_and_a_lamp_without_a_tile_its_radius() {
        let reach = cells_of(&wall(300.0), Some(64)).reach[0];
        assert!(reach[0] > 300.0 && reach[0] < 520.0, "{reach:?}");
        assert_eq!(reach[1], 0.0);
        assert_eq!(cells_of(&wall(300.0), None).reach, [[1000.0; 6]]);
    }

    #[test]
    fn a_grid_too_large_takes_larger_cells() {
        let far = |x: f32| CellLamp { position: [x, 0.0, 0.0], radius: 1000.0, face: None, casters: &[], strength: 1.0, spot: None };
        let cells = grid(&[far(0.0), far(200_000.0)], 128.0);
        assert_eq!(cells.cell, 256.0);
        assert!(cells.dims[0] <= MOST_CELLS_AN_AXIS);
    }

    #[test]
    fn the_book_points_each_lit_cell_at_its_lamps_four_a_texel() {
        let cells = LampCells { origin: [1.0, 2.0, 3.0], cell: 128.0, dims: [2, 1, 1], starts: vec![0, 0, 5], lamps: vec![0, 1, 1, 0, 1], in_reach: 5, reach: Vec::new() };
        let texels = book_texels(&cells, &[[[7.0; 4]; RECORD_TEXELS], [[9.0; 4]; RECORD_TEXELS]]);
        let cells_start = HEADER_TEXELS + 2 * RECORD_TEXELS;
        assert_eq!(texels[..2], [[1.0, 2.0, 3.0, 128.0], [2.0, 1.0, 1.0, cells_start as f32]]);
        assert_eq!(texels[HEADER_TEXELS + RECORD_TEXELS], [9.0; 4]);
        assert_eq!(texels[cells_start], [0.0; 4]);
        assert_eq!(texels[cells_start + 1], [(cells_start + 2) as f32, 5.0, 0.0, 0.0]);
        assert_eq!(texels[cells_start + 2..], [[0.0, 1.0, 1.0, 0.0], [1.0, 0.0, 0.0, 0.0]]);
    }
}
