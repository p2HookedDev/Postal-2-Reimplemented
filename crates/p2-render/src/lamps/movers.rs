//! What moves casts into tiles of its own: an atlas square for one mover seen from one lamp, redrawn every
//! frame the mover changes. The main world picks lamps and squares ([`super::mover_pick`]); the render world
//! draws the tiles and writes them into the book ([`super::mover_pass`]).

use std::ops::Range;

use bevy::asset::AssetId;
use bevy::ecs::prelude::Resource;
use bevy::math::{Mat4, Vec3};
use bevy::mesh::Mesh;
use bevy::render::RenderApp;
use bevy::render::extract_resource::ExtractResource;

/// As many tiles as the book's tiles' row holds, two texels a tile. `lamp_shadow.wesl` reads the row by its
/// place.
pub(super) const MOST_MOVER_TILES: usize = 1024;

pub(super) const TILE_MARGIN: f32 = 1.1;
pub(super) const WIDEST_TILE: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Drawn {
    Kept,
    Again,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MoverTile {
    pub(super) mover: u32,
    pub(super) lamp: u32,
    pub(super) lamp_at: Vec3,
    pub(super) lamp_radius: f32,
    pub(super) axis: Vec3,
    pub(super) tan_half: f32,
    pub(super) near: f32,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) size: u32,
    pub(super) fade: f32,
    pub(super) drawn: Drawn,
}

impl MoverTile {
    /// The tile's right and up, of length 1 and square to its axis. `lamp_shadow.wesl` and `mover_caster.wesl` make
    /// the same two from the axis.
    pub(super) fn sides(&self) -> (Vec3, Vec3) {
        let helper = if self.axis.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let right = helper.cross(self.axis).normalize();
        (right, self.axis.cross(right))
    }

    pub(super) fn texels(&self) -> [[f32; 4]; 2] {
        [[self.axis.x, self.axis.y, self.axis.z, self.tan_half], [self.x as f32, self.y as f32, self.size as f32, self.fade]]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MoverCaster {
    pub(super) mesh: AssetId<Mesh>,
    pub(super) world_from_mesh: Mat4,
    pub(super) joints: Option<Range<u32>>,
    pub(super) mover: u32,
}

#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct MoverTiles {
    pub(super) tiles: Vec<MoverTile>,
    pub(super) casters: Vec<MoverCaster>,
    pub(super) joints: Vec<Mat4>,
    pub(super) grounding: Vec<[f32; 4]>,
    pub(super) tile_grid: Vec<[f32; 4]>,
}

impl MoverTiles {
    pub fn counts(&self) -> (usize, usize) {
        (self.tiles.len(), self.tiles.chunk_by(|a, b| a.lamp == b.lamp).map(<[MoverTile]>::len).max().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tiles_sides_are_square_to_its_axis_and_to_each_other_whichever_way_it_looks() {
        for axis in [Vec3::X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::new(0.3, -0.92, 0.25).normalize(), Vec3::new(-0.6, 0.1, 0.79).normalize()] {
            let tile = MoverTile { mover: 0, lamp: 0, lamp_at: Vec3::ZERO, lamp_radius: 1.0, axis, tan_half: 0.5, near: 0.0, x: 0, y: 0, size: 64, fade: 1.0, drawn: Drawn::Again };
            let (right, up) = tile.sides();
            assert!((right.length() - 1.0).abs() < 1e-5 && (up.length() - 1.0).abs() < 1e-5, "{axis}");
            assert!(right.dot(axis).abs() < 1e-5 && up.dot(axis).abs() < 1e-5 && right.dot(up).abs() < 1e-5, "{axis}");
            assert!(right.cross(up).dot(axis) > 0.99, "{axis}");
        }
    }
}
