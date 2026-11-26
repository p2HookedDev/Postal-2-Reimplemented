use bevy::math::Vec3;

use super::cells::{BOOK_WIDTH, CAPSULES_FROM_END, TILES_FROM_END};

/// Columns to a side of the grid. `lamp_shadow.wesl` holds the same number.
const GRID_SIDE: usize = 32;
/// A column's side in world units in each grid, the fine one first: the rows the book ends with.
pub(super) const GRID_COLUMNS: [f32; 2] = [64.0, 256.0];
const _: () = assert!(GRID_COLUMNS.len() == CAPSULES_FROM_END as usize && TILES_FROM_END == CAPSULES_FROM_END + 1);
/// How far from its axis a capsule darkens, in radii. `lamp_shadow.wesl` holds the same number.
const REACH_RADII: f32 = 3.0;
const CAPSULES_AT: usize = 1 + GRID_SIDE * GRID_SIDE;
const MOST_CAPSULES: usize = (BOOK_WIDTH - CAPSULES_AT) / 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Capsule {
    low: Vec3,
    high: Vec3,
    radius: f32,
}

impl Capsule {
    pub(super) fn of_box(min: Vec3, max: Vec3) -> Self {
        let (middle, half) = (0.5 * (min + max), 0.5 * (max - min));
        let long = half.max_position();
        let radius = (0.5 * (half.element_sum() - half[long])).min(half[long]);
        let mut reach = Vec3::ZERO;
        reach[long] = half[long] - radius;
        Self { low: middle - reach, high: middle + reach, radius }
    }
}

pub(super) fn grounding_row(eye: Vec3, capsules: impl IntoIterator<Item = Capsule>, column_units: f32, rows: &mut Vec<[f32; 4]>) {
    let start = rows.len();
    rows.resize(start + BOOK_WIDTH, [0.0; 4]);
    let row = &mut rows[start..];
    let low = [eye.x, eye.z].map(|at| ((at / column_units).floor() - (GRID_SIDE / 2) as f32) * column_units);
    let mut count = 0;
    for capsule in capsules {
        if count == MOST_CAPSULES {
            break;
        }
        let reach = capsule.radius * REACH_RADII;
        let columns = |from: f32, to: f32, low: f32| {
            let column = |at: f32| ((at - low) / column_units).floor();
            let (first, last) = (column(from.min(to) - reach).max(0.0), column(from.max(to) + reach).min((GRID_SIDE - 1) as f32));
            first as usize..if last < first { first as usize } else { last as usize + 1 }
        };
        let mut held = false;
        for z in columns(capsule.low.z, capsule.high.z, low[1]) {
            for x in columns(capsule.low.x, capsule.high.x, low[0]) {
                if let Some(free) = row[1 + z * GRID_SIDE + x].iter_mut().find(|slot| **slot == 0.0) {
                    *free = (count + 1) as f32;
                    held = true;
                }
            }
        }
        if held {
            row[CAPSULES_AT + 2 * count] = [capsule.low.x, capsule.low.y, capsule.low.z, capsule.radius];
            row[CAPSULES_AT + 2 * count + 1] = [capsule.high.x, capsule.high.y, capsule.high.z, 0.0];
            count += 1;
        }
    }
    row[0] = [low[0], low[1], column_units, count as f32];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::shaders::shader_const;

    #[test]
    fn the_shader_reads_the_capsules_as_they_are_written() {
        let shader = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/lamp_shadow.wesl"));
        assert_eq!(shader_const(shader, "GROUND_SIDE"), GRID_SIDE as f64);
        assert_eq!(shader_const(shader, "GROUND_REACH") as f32, REACH_RADII);
    }

    fn column_of(row: &[[f32; 4]], at: Vec3) -> [f32; 4] {
        let [x, z] = [(at.x - row[0][0]) / row[0][2], (at.z - row[0][1]) / row[0][2]].map(|column| column.floor() as usize);
        row[1 + z * GRID_SIDE + x]
    }

    #[test]
    fn a_standing_box_is_an_upright_capsule_that_touches_its_ends() {
        let capsule = Capsule::of_box(Vec3::new(-20.0, 0.0, -20.0), Vec3::new(20.0, 80.0, 20.0));
        assert_eq!(capsule, Capsule { low: Vec3::new(0.0, 20.0, 0.0), high: Vec3::new(0.0, 60.0, 0.0), radius: 20.0 });
        let lying = Capsule::of_box(Vec3::new(0.0, 0.0, 0.0), Vec3::new(80.0, 20.0, 40.0));
        assert_eq!(lying, Capsule { low: Vec3::new(15.0, 10.0, 20.0), high: Vec3::new(65.0, 10.0, 20.0), radius: 15.0 });
    }

    #[test]
    fn a_column_names_the_capsules_in_its_reach_and_no_others() {
        let eye = Vec3::new(1000.0, 50.0, -500.0);
        let near = Capsule::of_box(eye + Vec3::new(80.0, -50.0, 0.0), eye + Vec3::new(120.0, 30.0, 40.0));
        let far = Capsule::of_box(eye + Vec3::new(5000.0, 0.0, 0.0), eye + Vec3::new(5040.0, 80.0, 40.0));
        let mut row = Vec::new();
        for column_units in GRID_COLUMNS {
            grounding_row(eye, [far, near], column_units, &mut row);
        }
        assert_eq!(row.len(), 2 * BOOK_WIDTH);
        assert_eq!((row[0][2..].to_vec(), row[BOOK_WIDTH][2..].to_vec()), (vec![GRID_COLUMNS[0], 1.0], vec![GRID_COLUMNS[1], 1.0]));
        assert_eq!(column_of(&row[BOOK_WIDTH..], near.low), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(column_of(&row, near.low), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(column_of(&row, near.low + Vec3::X * (near.radius * REACH_RADII - 1.0)), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(column_of(&row, near.low + Vec3::X * (near.radius * REACH_RADII + GRID_COLUMNS[0])), [0.0; 4]);
        assert_eq!(row[CAPSULES_AT], [near.low.x, near.low.y, near.low.z, near.radius]);
        assert_eq!(row[CAPSULES_AT + 1][..3], near.high.to_array());
    }

    #[test]
    fn a_full_column_keeps_the_first_four() {
        let eye = Vec3::ZERO;
        let capsule = Capsule::of_box(Vec3::new(-10.0, 0.0, -10.0), Vec3::new(10.0, 60.0, 10.0));
        let mut row = Vec::new();
        grounding_row(eye, [capsule; 6], GRID_COLUMNS[0], &mut row);
        assert_eq!(column_of(&row, capsule.low), [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(row[0][3], 4.0);
    }
}
