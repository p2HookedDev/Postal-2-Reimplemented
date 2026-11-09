use p2_assets::{Model, Terrain, poly_flags};
use p2_collision::{MeshCollision, Sides, line_check, line_check_node, mesh_line_check, terrain_line_check};
use p2_math::Vector;

pub const SUN_DISTANCE: f32 = 65536.0;

const MAX_TREE_DEPTH: usize = 64;

struct TreeNode {
    min: Vector,
    max: Vector,
    children: Option<[u32; 2]>,
    mesh: u32,
}

pub(crate) enum Seen {
    Open,
    Sky,
    Surface { time: f32, node: usize, location: Vector },
    Other { time: f32 },
}

pub struct Scene<'a> {
    pub(crate) model: &'a Model,
    terrains: &'a [Terrain],
    meshes: &'a [MeshCollision],
    tree: Vec<TreeNode>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MeshHit {
    Earliest,
    Any,
}

impl<'a> Scene<'a> {
    pub fn new(model: &'a Model, terrains: &'a [Terrain], meshes: &'a [MeshCollision]) -> Self {
        let mut scene = Self { model, terrains, meshes, tree: Vec::with_capacity(meshes.len() * 2) };
        let mut indices: Vec<u32> = (0..meshes.len() as u32).collect();
        if !indices.is_empty() {
            scene.build(&mut indices);
        }
        scene
    }

    fn build(&mut self, indices: &mut [u32]) -> u32 {
        let bounds = |index: u32| self.meshes[index as usize].bounds();
        let (mut min, mut max) = bounds(indices[0]);
        for &index in indices.iter() {
            let (low, high) = bounds(index);
            for axis in 0..3 {
                min[axis] = min[axis].min(low[axis]);
                max[axis] = max[axis].max(high[axis]);
            }
        }
        let at = self.tree.len() as u32;
        self.tree.push(TreeNode { min, max, children: None, mesh: indices[0] });
        if indices.len() > 1 {
            let extent = max - min;
            let axis = (0..3).fold(0, |best, axis| if extent[axis] > extent[best] { axis } else { best });
            let meshes = self.meshes;
            indices.sort_unstable_by(|&a, &b| {
                let centre = |index: u32| {
                    let (low, high) = meshes[index as usize].bounds();
                    low[axis] + high[axis]
                };
                centre(a).total_cmp(&centre(b))
            });
            let (left, right) = indices.split_at_mut(indices.len() / 2);
            let children = [self.build(left), self.build(right)];
            self.tree[at as usize].children = Some(children);
        }
        at
    }

    fn mesh_time(&self, start: Vector, end: Vector, wanted: MeshHit) -> Option<f32> {
        if self.tree.is_empty() {
            return None;
        }
        let mut best: Option<f32> = None;
        let mut stack = [0u32; MAX_TREE_DEPTH];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let node = &self.tree[stack[depth] as usize];
            if !crosses_box(start, end, node.min, node.max, best.unwrap_or(1.0)) {
                continue;
            }
            match node.children {
                Some([left, right]) if depth + 2 <= MAX_TREE_DEPTH => {
                    stack[depth] = left;
                    stack[depth + 1] = right;
                    depth += 2;
                }
                Some(_) => {}
                None => {
                    if let Some(hit) = mesh_line_check(std::iter::once(&self.meshes[node.mesh as usize]), start, end)
                        && best.is_none_or(|time| hit.time < time)
                    {
                        if wanted == MeshHit::Any {
                            return Some(hit.time);
                        }
                        best = Some(hit.time);
                    }
                }
            }
        }
        best
    }

    fn other_blocks(&self, start: Vector, end: Vector) -> bool {
        self.mesh_time(start, end, MeshHit::Any).is_some() || self.terrains.iter().any(|terrain| terrain_line_check(terrain, start, end, Sides::Both).is_some())
    }

    pub(crate) fn blocked(&self, start: Vector, end: Vector) -> bool {
        line_check(self.model, start, end).is_some() || self.other_blocks(start, end)
    }

    /// The sky counts as open: the sun's rays reach the world through backdrop surfaces.
    pub fn sun_clear(&self, start: Vector, to_sun: Vector) -> bool {
        let far = start + to_sun * SUN_DISTANCE;
        let end = match line_check_node(self.model, start, far) {
            None => far,
            Some((hit, Some(node))) if is_sky(self.model, node) => hit.location,
            Some(_) => return false,
        };
        !self.other_blocks(start, end)
    }

    pub(crate) fn closest(&self, start: Vector, end: Vector) -> Seen {
        let (surface, stop) = match line_check_node(self.model, start, end) {
            None => (None, 1.0),
            Some((hit, node)) => (Some((hit, node)), hit.time),
        };
        let other = self
            .terrains
            .iter()
            .filter_map(|terrain| terrain_line_check(terrain, start, end, Sides::Both).map(|hit| hit.time))
            .chain(self.mesh_time(start, end, MeshHit::Earliest))
            .fold(f32::MAX, f32::min);
        if other < stop {
            return Seen::Other { time: other };
        }
        match surface {
            None => Seen::Open,
            Some((_, Some(node))) if is_sky(self.model, node) => Seen::Sky,
            Some((hit, Some(node))) => Seen::Surface { time: hit.time, node, location: hit.location },
            Some((hit, None)) => Seen::Other { time: hit.time },
        }
    }
}

pub fn is_sky(model: &Model, node: usize) -> bool {
    model.nodes.get(node).and_then(|node| model.surfaces.get(node.surface)).is_some_and(|surface| surface.poly_flags & poly_flags::FAKE_BACKDROP != 0)
}

fn crosses_box(start: Vector, end: Vector, min: Vector, max: Vector, limit: f32) -> bool {
    let (mut near, mut far) = (0.0f32, limit);
    for axis in 0..3 {
        let delta = end[axis] - start[axis];
        if delta.abs() < 1e-6 {
            if start[axis] < min[axis] || start[axis] > max[axis] {
                return false;
            }
            continue;
        }
        let (a, b) = ((min[axis] - start[axis]) / delta, (max[axis] - start[axis]) / delta);
        near = near.max(a.min(b));
        far = far.min(a.max(b));
        if near > far {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use p2_math::Rotator;

    use super::*;

    fn slab(x: f32) -> MeshCollision {
        let triangles = [[[x, -8.0, -8.0], [x, 8.0, -8.0], [x, 8.0, 8.0]], [[x, -8.0, -8.0], [x, 8.0, 8.0], [x, -8.0, 8.0]]];
        MeshCollision::placed(triangles, Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)
    }

    #[test]
    fn the_tree_finds_what_a_full_scan_finds() {
        let meshes: Vec<_> = (0..40).map(|i| slab(i as f32 * 32.0)).collect();
        let model = Model::default();
        let scene = Scene::new(&model, &[], &meshes);
        let (start, end) = (Vector::new(-16.0, 1.0, 1.0), Vector::new(2000.0, 1.0, 1.0));
        let full = mesh_line_check(&meshes, start, end).map(|hit| hit.time);
        assert_eq!(scene.mesh_time(start, end, MeshHit::Earliest), full);
        assert!(scene.blocked(start, end));
        assert!(!scene.blocked(Vector::new(-16.0, 50.0, 1.0), Vector::new(2000.0, 50.0, 1.0)));
        assert!(!scene.blocked(Vector::new(-16.0, 1.0, 1.0), Vector::new(-8.0, 1.0, 1.0)));
    }
}
