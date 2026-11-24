//! Landed liquid as volume on a grid of floor cells: it pours in, runs downhill, keeps a film, spills over ledges and
//! dries. Unreal units and axes throughout; a cell's volume is its depth times its area.

use p2_math::Vector;
use p2_vm::FluidType;
use rustc_hash::FxHashMap;

use crate::level_collision::{LevelCollision, StaticCollision};

/// A cell's side in Unreal units.
pub const CELL: f32 = 8.0;
/// Cells along a chunk's side.
pub const CHUNK: i32 = 32;
const CELLS: usize = (CHUNK * CHUNK) as usize;
const AREA: f32 = CELL * CELL;
/// Pools on floors this far apart in height keep to chunks of their own.
const STOREY: f32 = 128.0;
/// The depth a wet cell keeps: flow never takes a cell below it.
pub const FILM: f32 = 0.05;
/// Below this a cell is dry.
const DRY: f32 = 0.002;
/// The share of a height difference that moves to one neighbour in a step; at most a quarter keeps the flow steady.
const FLOW: f32 = 0.2;
/// A neighbour whose floor lies further below than this is over a ledge: what runs there spills.
const LEDGE: f32 = 12.0;
/// How far above a wet cell's floor a neighbour's floor ray starts, so a kerb is still a floor.
const STEP_UP: f32 = 6.0;
const RAY_REACH: f32 = 256.0;
const MOST_RAYS_PER_STEP: usize = 64;
/// A chunk poured in this long is never evicted.
const FRESH: f32 = 2.0;
/// A chunk whose largest move in a step is under this sleeps until poured or flowed into.
const STILL: f32 = 1e-5;
/// The deepest a cell is drawn; deeper reads the same.
pub const MOST_DRAWN_DEPTH: f32 = 2.0;
/// Blood past this share of its budget dries this many times faster.
const BLOOD_OVER: f32 = 1.25;
const BLOOD_OVER_DRYING: f32 = 4.0;

pub trait Floor {
    /// The height of the floor straight under `from` within `reach`; `None` where there is none or `from` is inside.
    fn floor_below(&self, from: Vector, reach: f32) -> Option<f32>;
}

impl Floor for StaticCollision {
    fn floor_below(&self, from: Vector, reach: f32) -> Option<f32> {
        let hit = self.first_hit(from, from - Vector::new(0.0, 0.0, reach))?;
        (hit.time > 0.0 && hit.normal.z > 0.5).then_some(hit.location.z)
    }
}

impl Floor for LevelCollision {
    fn floor_below(&self, from: Vector, reach: f32) -> Option<f32> {
        self.0.as_ref()?.floor_below(from, reach)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub depth: f32,
    /// NaN until its ray is cast; infinite where liquid cannot go.
    pub floor: f32,
    /// The share of the depth that is blood.
    pub blood: f32,
    pub kind: Option<FluidType>,
}

impl Default for Cell {
    fn default() -> Self {
        Self { depth: 0.0, floor: f32::NAN, blood: 0.0, kind: None }
    }
}

impl Cell {
    fn take(&mut self, volume: f32, kind: FluidType, blood: f32) {
        let depth = volume / AREA;
        let total = self.depth + depth;
        if total > 0.0 {
            self.blood = (self.blood * self.depth + blood * depth) / total;
        }
        if self.kind.is_none() || depth > self.depth {
            self.kind = Some(kind);
        }
        self.depth = total;
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub cells: Vec<Cell>,
    /// The level time it was last poured into.
    pub last_pour: f32,
    pub awake: bool,
    /// One bit per row of cells whose drawn depth changed since the bits were last taken.
    pub dirty_rows: u32,
}

impl Chunk {
    fn new(now: f32) -> Self {
        Self { cells: vec![Cell::default(); CELLS], last_pour: now, awake: true, dirty_rows: 0 }
    }

    pub fn volume(&self) -> f32 {
        self.cells.iter().map(|cell| cell.depth).sum::<f32>() * AREA
    }
}

/// Liquid that ran over a ledge, to be poured again where it lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spill {
    pub at: Vector,
    pub kind: FluidType,
    pub volume: f32,
}

#[derive(Debug, Clone, Copy)]
struct Move {
    key: (i32, i32, i32),
    index: usize,
    volume: f32,
    kind: FluidType,
    blood: f32,
}

pub fn drawn_depth(depth: f32) -> u8 {
    (depth / MOST_DRAWN_DEPTH * 255.0).round().clamp(0.0, 255.0) as u8
}

fn is_blood(kind: FluidType) -> bool {
    matches!(kind, FluidType::Blood | FluidType::BloodyPuke | FluidType::BloodyUrine)
}

fn place(at: Vector) -> ([i32; 2], i32) {
    ([(at.x / CELL).floor() as i32, (at.y / CELL).floor() as i32], (at.z / STOREY).floor() as i32)
}

pub(crate) fn locate([x, y]: [i32; 2], storey: i32) -> ((i32, i32, i32), usize) {
    let key = (x.div_euclid(CHUNK), y.div_euclid(CHUNK), storey);
    (key, (y.rem_euclid(CHUNK) * CHUNK + x.rem_euclid(CHUNK)) as usize)
}

fn cell_of(key: (i32, i32, i32), index: usize) -> [i32; 2] {
    let index = index as i32;
    [key.0 * CHUNK + index % CHUNK, key.1 * CHUNK + index / CHUNK]
}

fn middle([x, y]: [i32; 2], z: f32) -> Vector {
    Vector::new((x as f32 + 0.5) * CELL, (y as f32 + 0.5) * CELL, z)
}

const NEIGHBOURS: [[i32; 2]; 4] = [[1, 0], [-1, 0], [0, 1], [0, -1]];

#[derive(Debug, Clone)]
pub struct PoolStore {
    /// Keyed by the chunk's x, y and storey.
    pub chunks: FxHashMap<(i32, i32, i32), Chunk>,
    pub most_chunks: usize,
    /// Blood volume past which blood dries faster.
    pub blood_budget: f32,
    moves: Vec<Move>,
    rays: Vec<((i32, i32, i32), usize, f32)>,
}

impl PoolStore {
    pub fn new(most_chunks: usize, blood_budget: f32) -> Self {
        Self { chunks: FxHashMap::default(), most_chunks, blood_budget, moves: Vec::new(), rays: Vec::new() }
    }

    pub fn volume(&self) -> f32 {
        self.chunks.values().map(Chunk::volume).sum()
    }

    pub fn cell(&self, at: Vector) -> Option<&Cell> {
        let (cell, storey) = place(at);
        let (key, index) = locate(cell, storey);
        self.chunks.get(&key).map(|chunk| &chunk.cells[index])
    }

    /// The chunk at `key`, made if there is room or a quiet chunk to evict; `None` when every chunk is fresh.
    fn chunk_at(&mut self, key: (i32, i32, i32), now: f32) -> Option<&mut Chunk> {
        if !self.chunks.contains_key(&key) && self.chunks.len() >= self.most_chunks {
            let quietest = self.chunks.iter().filter(|(_, chunk)| now - chunk.last_pour > FRESH).min_by(|a, b| a.1.last_pour.total_cmp(&b.1.last_pour)).map(|(&key, _)| key)?;
            self.chunks.retain(|&other, _| other != quietest);
        }
        Some(self.chunks.entry(key).or_insert_with(|| Chunk::new(now)))
    }

    /// Pours `volume` onto the floor at `at`; dropped when every chunk is fresh and the store is full.
    pub fn pour(&mut self, at: Vector, kind: FluidType, volume: f32, now: f32) {
        self.add(at, kind, volume, now, f32::INFINITY);
    }

    /// Tops the cell at `at` up to `depth` and no more, as a trail wets the floor it runs along.
    pub fn film(&mut self, at: Vector, kind: FluidType, depth: f32, now: f32) {
        self.add(at, kind, depth * AREA, now, depth);
    }

    fn add(&mut self, at: Vector, kind: FluidType, volume: f32, now: f32, up_to: f32) {
        let (cell, storey) = place(at);
        let (key, index) = locate(cell, storey);
        let Some(chunk) = self.chunk_at(key, now) else { return };
        chunk.last_pour = now;
        chunk.awake = true;
        let cell = &mut chunk.cells[index];
        if !cell.floor.is_finite() {
            cell.floor = at.z;
        }
        let volume = volume.min((up_to - cell.depth).max(0.0) * AREA);
        let before = drawn_depth(cell.depth);
        cell.take(volume, kind, if is_blood(kind) { 1.0 } else { 0.0 });
        if drawn_depth(cell.depth) != before {
            chunk.dirty_rows |= 1 << (index as i32 / CHUNK);
        }
    }

    /// One step of `seconds`: floor rays, flow, drying at `drying` depth a second. Returns what spilled over ledges.
    pub fn step(&mut self, seconds: f32, floor: &impl Floor, drying: f32, now: f32, spills: &mut Vec<Spill>) {
        self.cast_floors(floor, now);
        self.flow(spills);
        self.dry(seconds, drying, now);
    }

    fn cast_floors(&mut self, floor: &impl Floor, now: f32) {
        let mut rays = std::mem::take(&mut self.rays);
        rays.sort_unstable_by_key(|&(key, index, _)| (key, index));
        rays.dedup_by_key(|&mut (key, index, _)| (key, index));
        for &(key, index, from) in rays.iter().take(MOST_RAYS_PER_STEP) {
            let Some(chunk) = self.chunk_at(key, now) else { continue };
            let cell = &mut chunk.cells[index];
            if cell.floor.is_nan() {
                let below = floor.floor_below(middle(cell_of(key, index), from + STEP_UP), RAY_REACH);
                cell.floor = below.unwrap_or(f32::INFINITY);
                chunk.awake = true;
            }
        }
        rays.clear();
        self.rays = rays;
    }

    fn flow(&mut self, spills: &mut Vec<Spill>) {
        let mut moves = std::mem::take(&mut self.moves);
        moves.clear();
        let mut waiting = Vec::new();
        for (&key, chunk) in self.chunks.iter().filter(|(_, chunk)| chunk.awake) {
            for (index, cell) in chunk.cells.iter().enumerate() {
                let Some(kind) = cell.kind.filter(|_| cell.depth > FILM && cell.floor.is_finite()) else { continue };
                let [x, y] = cell_of(key, index);
                let height = cell.floor + cell.depth;
                let mut wants = [0.0; 4];
                let mut targets = [(key, 0); 4];
                let mut ledges = [None; 4];
                for (k, [dx, dy]) in NEIGHBOURS.into_iter().enumerate() {
                    let (other_key, other_index) = locate([x + dx, y + dy], key.2);
                    targets[k] = (other_key, other_index);
                    let other = self.chunks.get(&other_key).map_or(Cell::default(), |chunk| chunk.cells[other_index]);
                    if other.floor.is_nan() {
                        self.rays.push((other_key, other_index, cell.floor));
                        waiting.push(key);
                    } else if cell.floor - other.floor > LEDGE {
                        ledges[k] = Some(other.floor);
                        wants[k] = (cell.depth - FILM) * FLOW;
                    } else {
                        wants[k] = (height - other.floor - other.depth).max(0.0) * FLOW;
                    }
                }
                let total: f32 = wants.iter().sum();
                if total <= 0.0 {
                    continue;
                }
                let scale = ((cell.depth - FILM) / total).min(1.0) * AREA;
                moves.push(Move { key, index, volume: -total * scale, kind, blood: cell.blood });
                for (k, &(other_key, other_index)) in targets.iter().enumerate().filter(|&(k, _)| wants[k] > 0.0) {
                    let volume = wants[k] * scale;
                    match ledges[k] {
                        Some(below) => spills.push(Spill { at: middle(cell_of(other_key, other_index), below), kind, volume }),
                        None => moves.push(Move { key: other_key, index: other_index, volume, kind, blood: cell.blood }),
                    }
                }
            }
        }
        for (key, chunk) in self.chunks.iter_mut().filter(|(_, chunk)| chunk.awake) {
            chunk.awake = waiting.contains(key);
        }
        for &Move { key, index, volume, kind, blood } in &moves {
            let Some(chunk) = self.chunks.get_mut(&key) else { continue };
            let cell = &mut chunk.cells[index];
            let before = drawn_depth(cell.depth);
            if volume < 0.0 {
                cell.depth = (cell.depth + volume / AREA).max(0.0);
            } else {
                cell.take(volume, kind, blood);
            }
            if drawn_depth(cell.depth) != before {
                chunk.dirty_rows |= 1 << (index as i32 / CHUNK);
            }
            chunk.awake |= volume.abs() > STILL * AREA;
        }
        self.moves = moves;
    }

    fn dry(&mut self, seconds: f32, drying: f32, now: f32) {
        let blood: f32 = self.chunks.values().flat_map(|chunk| &chunk.cells).map(|cell| cell.depth * cell.blood * AREA).sum();
        let blood_drying = if blood > self.blood_budget * BLOOD_OVER { BLOOD_OVER_DRYING } else { 1.0 };
        let loss = drying * seconds;
        for chunk in self.chunks.values_mut().filter(|_| loss > 0.0) {
            for (index, cell) in chunk.cells.iter_mut().enumerate().filter(|(_, cell)| cell.depth > 0.0) {
                let before = drawn_depth(cell.depth);
                cell.depth -= loss * (1.0 + cell.blood * (blood_drying - 1.0));
                if cell.depth < DRY {
                    *cell = Cell { floor: cell.floor, ..Cell::default() };
                }
                if drawn_depth(cell.depth) != before {
                    chunk.dirty_rows |= 1 << (index as i32 / CHUNK);
                }
            }
        }
        self.chunks.retain(|_, chunk| now - chunk.last_pour <= FRESH || chunk.cells.iter().any(|cell| cell.depth > 0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Slope(f32);

    impl Floor for Slope {
        fn floor_below(&self, from: Vector, _reach: f32) -> Option<f32> {
            Some(-from.x * self.0)
        }
    }

    fn settle(store: &mut PoolStore, floor: &impl Floor, steps: usize) -> Vec<Spill> {
        let mut spills = Vec::new();
        for _ in 0..steps {
            store.step(1.0 / 30.0, floor, 0.0, 0.0, &mut spills);
        }
        spills
    }

    fn wet(store: &PoolStore) -> impl Iterator<Item = ([i32; 2], &Cell)> {
        store.chunks.iter().flat_map(|(&key, chunk)| chunk.cells.iter().enumerate().map(move |(index, cell)| (cell_of(key, index), cell))).filter(|(_, cell)| cell.depth > 0.0)
    }

    #[test]
    fn flow_keeps_the_volume_poured() {
        let mut store = PoolStore::new(64, 1e6);
        store.pour(Vector::new(4.0, 4.0, 0.0), FluidType::Urine, 500.0, 0.0);
        let spills = settle(&mut store, &Slope(0.0), 300);
        assert!(spills.is_empty());
        assert!((store.volume() - 500.0).abs() < 0.01, "{}", store.volume());
        assert!(wet(&store).count() > 9);
    }

    #[test]
    fn a_slope_drains_downhill() {
        let mut store = PoolStore::new(64, 1e6);
        store.pour(Vector::new(4.0, 4.0, 0.0), FluidType::Blood, 300.0, 0.0);
        settle(&mut store, &Slope(0.1), 300);
        let (moment, volume) = wet(&store).fold((0.0, 0.0), |(m, v), ([x, _], cell)| (m + x as f32 * cell.depth, v + cell.depth));
        assert!(moment / volume > 1.0, "{}", moment / volume);
    }

    #[test]
    fn the_film_stays() {
        let mut store = PoolStore::new(64, 1e6);
        let at = Vector::new(4.0, 4.0, 0.0);
        store.pour(at, FluidType::Gas, FILM * AREA * 3.0, 0.0);
        settle(&mut store, &Slope(0.0), 600);
        assert!(store.cell(at).is_some_and(|cell| cell.depth >= FILM * 0.999));
        assert!(wet(&store).count() <= 9);
    }

    #[test]
    fn a_film_tops_up_and_no_more() {
        let mut store = PoolStore::new(64, 1e6);
        let at = Vector::new(4.0, 4.0, 0.0);
        store.film(at, FluidType::Urine, 0.1, 0.0);
        store.film(at, FluidType::Urine, 0.1, 0.0);
        assert!(store.cell(at).is_some_and(|cell| (cell.depth - 0.1).abs() < 1e-6));
    }

    #[test]
    fn a_fresh_pool_is_never_evicted() {
        let mut store = PoolStore::new(1, 1e6);
        let (near, far) = (Vector::new(4.0, 4.0, 0.0), Vector::new(4000.0, 4.0, 0.0));
        store.pour(near, FluidType::Blood, 50.0, 0.0);
        store.pour(far, FluidType::Blood, 50.0, 1.0);
        assert!(store.cell(near).is_some() && store.cell(far).is_none());
        store.pour(far, FluidType::Blood, 50.0, 5.0);
        assert!(store.cell(near).is_none() && store.cell(far).is_some());
    }

    #[test]
    fn a_ledge_spills() {
        struct Ledge;
        impl Floor for Ledge {
            fn floor_below(&self, from: Vector, _reach: f32) -> Option<f32> {
                Some(if from.x > 8.0 { -100.0 } else { 0.0 })
            }
        }
        let mut store = PoolStore::new(64, 1e6);
        store.pour(Vector::new(4.0, 4.0, 0.0), FluidType::Urine, 200.0, 0.0);
        let spills = settle(&mut store, &Ledge, 120);
        let spilt: f32 = spills.iter().map(|spill| spill.volume).sum();
        assert!(spilt > 0.0 && spills.iter().all(|spill| spill.at.z == -100.0));
        assert!((store.volume() + spilt - 200.0).abs() < 0.01);
    }

    #[test]
    fn pools_dry_away() {
        let mut store = PoolStore::new(64, 1e6);
        store.pour(Vector::new(4.0, 4.0, 0.0), FluidType::Blood, 10.0, 0.0);
        let mut spills = Vec::new();
        for step in 0..100 {
            store.step(0.1, &Slope(0.0), 0.1, 3.0 + step as f32 * 0.1, &mut spills);
        }
        assert!(store.chunks.is_empty());
    }
}
