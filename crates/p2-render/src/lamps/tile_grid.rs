//! Which of the movers' tiles can shade each column of a grid around the eye, so a pixel walks only those its column
//! names. `lamp_shadow.wesl` reads the grid from the rows before the tiles' row.

use bevy::math::{Vec2, Vec3};

use super::cells::{BOOK_WIDTH, TILE_GRID_FROM_END, TILES_FROM_END};
use super::movers::{MOST_MOVER_TILES, MoverTile};

/// Columns to a side of the grid. `lamp_shadow.wesl` holds the same number.
const GRID_SIDE: usize = 64;
const COLUMN_UNITS: f32 = 128.0;
/// Texels a column, four tile names each: in a fight the busiest columns are crossed by twenty tiles and more.
/// `lamp_shadow.wesl` holds the same number.
const COLUMN_TEXELS: usize = 8;
const GRID_ROWS: usize = (1 + COLUMN_TEXELS * GRID_SIDE * GRID_SIDE).div_ceil(BOOK_WIDTH);
const _: () = assert!(GRID_ROWS == (TILE_GRID_FROM_END - TILES_FROM_END) as usize && 2 * MOST_MOVER_TILES <= BOOK_WIDTH);
/// How much wider than its tangent `lamp_shadow.wesl` lets a tile reach. `lamp_shadow.wesl` holds the same number.
const REACH_WIDENING: f32 = 1.4143;

/// Fills `rows` with the grid: its low corner in x and z, a column's side and 1; then `COLUMN_TEXELS` a column, naming
/// up to four times as many tiles by their place in `tiles` from 1, or -1 first when more cross it. A column may name a
/// tile that does not shade it, never the reverse. Leaves `rows` as they are when `made_from` holds the bits of the same
/// inputs, and says whether it built them.
pub(super) fn tile_grid_rows(eye: Vec3, tiles: &[MoverTile], rows: &mut Vec<[f32; 4]>, made_from: &mut Vec<u32>) -> bool {
    let low = Vec2::from([eye.x, eye.z].map(|at| ((at / COLUMN_UNITS).floor() - (GRID_SIDE / 2) as f32) * COLUMN_UNITS));
    let tiles = &tiles[..tiles.len().min(MOST_MOVER_TILES)];
    let inputs = || {
        let read = tiles.iter().flat_map(|tile| [tile.lamp_at.x, tile.lamp_at.z, tile.axis.x, tile.axis.z, tile.tan_half, tile.near, tile.lamp_radius]);
        low.to_array().into_iter().chain(read).map(f32::to_bits)
    };
    if rows.len() == GRID_ROWS * BOOK_WIDTH && made_from.iter().copied().eq(inputs()) {
        return false;
    }
    made_from.clear();
    made_from.extend(inputs());
    rows.clear();
    rows.resize(GRID_ROWS * BOOK_WIDTH, [0.0; 4]);
    rows[0] = [low.x, low.y, COLUMN_UNITS, 1.0];
    let slack = COLUMN_UNITS * std::f32::consts::FRAC_1_SQRT_2;
    for (index, tile) in tiles.iter().enumerate() {
        let name = (index + 1) as f32;
        let (from, axis) = (Vec2::new(tile.lamp_at.x, tile.lamp_at.z), Vec2::new(tile.axis.x, tile.axis.z));
        let widening = tile.tan_half * REACH_WIDENING;
        let (near, far) = (tile.near.min(tile.lamp_radius), tile.lamp_radius);
        let ends = [near, far].map(|along| (from + axis * along, along * widening + slack));
        let columns = |side: usize| {
            let column = |at: f32| ((at - low[side]) / COLUMN_UNITS).floor();
            let first = column(ends.iter().map(|(centre, reach)| centre[side] - reach).fold(f32::MAX, f32::min)).max(0.0);
            let last = column(ends.iter().map(|(centre, reach)| centre[side] + reach).fold(f32::MIN, f32::max)).min((GRID_SIDE - 1) as f32);
            first as usize..if last < first { first as usize } else { last as usize + 1 }
        };
        let flat = axis.length_squared();
        for z in columns(1) {
            for x in columns(0) {
                let to = low + (Vec2::new(x as f32, z as f32) + 0.5) * COLUMN_UNITS - from;
                // The cone's circle that comes nearest the column's middle. The gap to a circle is convex along the axis,
                // so it is least where its slope is 0, or at an end.
                let along = if flat <= widening * widening {
                    far
                } else {
                    let beside = (to.length_squared() - to.dot(axis).powi(2) / flat).max(0.0).sqrt();
                    (to.dot(axis) / flat + widening * beside / (flat * (flat - widening * widening)).sqrt()).clamp(near, far)
                };
                if (to - axis * along).length() > along * widening + slack {
                    continue;
                }
                let column = rows[1 + COLUMN_TEXELS * (z * GRID_SIDE + x)..][..COLUMN_TEXELS].as_flattened_mut();
                if column[0] < 0.0 {
                    continue;
                }
                match column.iter_mut().find(|slot| **slot == 0.0) {
                    Some(free) => *free = name,
                    None => column[0] = -1.0,
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::movers::Drawn;
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_shader_reads_the_grid_as_it_is_written() {
        let shader = include_str!("../lamp_shadow.wesl");
        assert_eq!(shader_const(shader, "TILE_GRID_SIDE"), GRID_SIDE as f64);
        assert_eq!(shader_const(shader, "TILE_GRID_COLUMN_TEXELS"), COLUMN_TEXELS as f64);
        assert_eq!(shader_const(shader, "REACH_WIDENING") as f32, REACH_WIDENING);
    }

    fn crossing_tile(lamp_at: Vec3, middle: Vec3) -> MoverTile {
        let (radius, distance) = (50.0, lamp_at.distance(middle));
        let tan_half = 1.1 * radius / (distance * distance - radius * radius).sqrt();
        MoverTile { mover: 0, lamp: 0, lamp_at, lamp_radius: 2000.0, axis: (middle - lamp_at) / distance, tan_half, near: distance - radius, x: 0, y: 0, size: 64, fade: 1.0, drawn: Drawn::Again }
    }

    fn named_by(rows: &[[f32; 4]], at: Vec3) -> Vec<f32> {
        let [x, z] = [(at.x - rows[0][0]) / rows[0][2], (at.z - rows[0][1]) / rows[0][2]].map(|column| column.floor() as usize);
        rows[1 + COLUMN_TEXELS * (z * GRID_SIDE + x)..][..COLUMN_TEXELS].as_flattened().to_vec()
    }

    #[test]
    fn a_column_names_the_tiles_whose_shadow_crosses_it_and_no_others() {
        let (lamp, middle) = (Vec3::new(-200.0, 300.0, 0.0), Vec3::new(400.0, 50.0, 0.0));
        let mut rows = Vec::new();
        tile_grid_rows(Vec3::ZERO, &[crossing_tile(lamp, middle)], &mut rows, &mut Vec::new());
        assert_eq!(rows.len(), GRID_ROWS * BOOK_WIDTH);
        assert_eq!(rows[0], [-4096.0, -4096.0, COLUMN_UNITS, 1.0]);
        assert_eq!(named_by(&rows, middle)[0], 1.0);
        assert_eq!(named_by(&rows, Vec3::new(1400.0, 0.0, 0.0))[0], 1.0);
        assert!(!named_by(&rows, Vec3::new(400.0, 0.0, 1500.0)).contains(&1.0));
        assert!(!named_by(&rows, Vec3::new(-100.0, 0.0, 0.0)).contains(&1.0));
    }

    #[test]
    fn a_column_crossed_by_more_tiles_than_it_names_starts_with_minus_one() {
        let tile = crossing_tile(Vec3::new(-200.0, 300.0, 0.0), Vec3::new(400.0, 50.0, 0.0));
        let names = 4 * COLUMN_TEXELS;
        let mut rows = Vec::new();
        tile_grid_rows(Vec3::ZERO, &vec![tile; names], &mut rows, &mut Vec::new());
        assert_eq!(named_by(&rows, Vec3::new(400.0, 0.0, 0.0)), (1..=names).map(|name| name as f32).collect::<Vec<_>>());
        tile_grid_rows(Vec3::ZERO, &vec![tile; names + 1], &mut rows, &mut Vec::new());
        assert_eq!(named_by(&rows, Vec3::new(400.0, 0.0, 0.0))[0], -1.0);
    }

    #[test]
    fn the_same_inputs_leave_the_grid_and_a_changed_tile_builds_it_again() {
        let tile = crossing_tile(Vec3::new(-200.0, 300.0, 0.0), Vec3::new(400.0, 50.0, 0.0));
        let (mut rows, mut made_from) = (Vec::new(), Vec::new());
        assert!(tile_grid_rows(Vec3::ZERO, &[tile], &mut rows, &mut made_from));
        rows[1] = [9.0; 4];
        assert!(!tile_grid_rows(Vec3::new(10.0, 50.0, 10.0), &[tile], &mut rows, &mut made_from));
        assert_eq!(rows[1], [9.0; 4]);
        let moved = MoverTile { near: tile.near + 1.0, ..tile };
        assert!(tile_grid_rows(Vec3::ZERO, &[moved], &mut rows, &mut made_from));
        let mut fresh = Vec::new();
        tile_grid_rows(Vec3::ZERO, &[moved], &mut fresh, &mut Vec::new());
        assert_eq!(rows, fresh);
        assert!(tile_grid_rows(Vec3::new(200.0, 0.0, 0.0), &[moved], &mut rows, &mut made_from));
    }

    #[test]
    fn no_column_a_tiles_cone_reaches_goes_unnamed() {
        let mut seed = 7u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0
        };
        for _ in 0..300 {
            let lamp_at = Vec3::new((next() - 0.5) * 6000.0, next() * 300.0, (next() - 0.5) * 6000.0);
            let axis = Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5).try_normalize().unwrap_or(Vec3::NEG_Y);
            let tile = MoverTile { axis, tan_half: 0.05 + next() * 1.95, near: next() * 600.0, lamp_radius: 100.0 + next() * 2500.0, ..crossing_tile(lamp_at, lamp_at + axis * 100.0) };
            let mut rows = Vec::new();
            tile_grid_rows(Vec3::ZERO, &[tile], &mut rows, &mut Vec::new());
            let (low, side) = (Vec2::new(rows[0][0], rows[0][1]), GRID_SIDE as f32 * COLUMN_UNITS);
            let near = tile.near.min(tile.lamp_radius);
            for step in 0..=200 {
                let along = near + (tile.lamp_radius - near) * step as f32 / 200.0;
                let centre = Vec2::new(lamp_at.x + axis.x * along, lamp_at.z + axis.z * along);
                for turn in 0..16 {
                    let rim = centre + Vec2::from_angle(turn as f32 * std::f32::consts::FRAC_PI_8) * (along * tile.tan_half * REACH_WIDENING * (turn % 3) as f32 * 0.5);
                    if rim.cmpge(low).all() && rim.cmplt(low + side).all() {
                        assert!(named_by(&rows, Vec3::new(rim.x, 0.0, rim.y)).contains(&1.0), "{tile:?} at {along} misses {rim}");
                    }
                }
            }
        }
    }
}
