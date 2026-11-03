use p2_math::Vector;

const LEAF_TRIANGLES: usize = 8;

/// World units a node's box grows by for a segment: far above the rounding of a world coordinate, so a hit
/// `segment_triangle` reports a hair off its triangle, or a hair past the segment's end, stays inside the box.
const SEGMENT_MARGIN: f32 = 1.0;

#[derive(Debug, Clone, Copy)]
struct Node {
    min: [f32; 3],
    max: [f32; 3],
    first: u32,
    count: u32,
}

impl Node {
    /// The line time at which a segment from `start`, with `inverse` the reciprocal of its motion on each axis, enters
    /// this box grown by [`SEGMENT_MARGIN`]; `None` if it does not by `limit`. On an axis the segment does not move
    /// along, a start outside the slab gives equal infinities and one on its face a NaN that `min` and `max` skip:
    /// either way an infinite bound rejects the box.
    fn entry(&self, start: Vector, inverse: [f32; 3], limit: f32) -> Option<f32> {
        let (mut enter, mut exit) = (0.0_f32, limit);
        for i in 0..3 {
            let a = (self.min[i] - SEGMENT_MARGIN - start[i]) * inverse[i];
            let b = (self.max[i] + SEGMENT_MARGIN - start[i]) * inverse[i];
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
        }
        (enter <= exit).then_some(enter)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TriangleTree {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

impl TriangleTree {
    pub(crate) fn new(triangles: &[[[f32; 3]; 3]]) -> Self {
        let mut tree = Self { nodes: Vec::new(), order: (0..triangles.len() as u32).collect() };
        if !triangles.is_empty() {
            tree.build(triangles, 0, triangles.len());
        }
        tree
    }

    fn build(&mut self, triangles: &[[[f32; 3]; 3]], first: usize, count: usize) {
        let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for &triangle in &self.order[first..first + count] {
            for corner in &triangles[triangle as usize] {
                for (axis, &value) in corner.iter().enumerate() {
                    min[axis] = min[axis].min(value);
                    max[axis] = max[axis].max(value);
                }
            }
        }
        let index = self.nodes.len();
        self.nodes.push(Node { min, max, first: first as u32, count: count as u32 });
        if count <= LEAF_TRIANGLES {
            return;
        }
        let axis = (0..3).fold(0, |longest, axis| if max[axis] - min[axis] > max[longest] - min[longest] { axis } else { longest });
        let middle = |triangle: u32| triangles[triangle as usize].iter().map(|corner| corner[axis]).sum::<f32>();
        let half = count / 2;
        self.order[first..first + count].select_nth_unstable_by(half, |&a, &b| middle(a).total_cmp(&middle(b)));
        self.build(triangles, first, half);
        self.nodes[index].first = self.nodes.len() as u32;
        self.nodes[index].count = 0;
        self.build(triangles, first + half, count - half);
    }

    pub(crate) fn any(&self, min: Vector, max: Vector, mut visit: impl FnMut(usize) -> bool) -> bool {
        let mut stack = [0u32; 64];
        let mut top = usize::from(!self.nodes.is_empty());
        while top > 0 {
            top -= 1;
            let index = stack[top];
            let node = &self.nodes[index as usize];
            if !(0..3).all(|i| node.min[i] <= max[i] && node.max[i] >= min[i]) {
                continue;
            }
            if node.count == 0 {
                stack[top] = node.first;
                stack[top + 1] = index + 1;
                top += 2;
            } else {
                let first = node.first as usize;
                for &triangle in &self.order[first..first + node.count as usize] {
                    if visit(triangle as usize) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub(crate) fn along(&self, start: Vector, end: Vector, mut limit: f32, mut visit: impl FnMut(usize) -> f32) {
        let inverse = [0, 1, 2].map(|i| 1.0 / (end[i] - start[i]));
        let entry = |node: u32, limit: f32| self.nodes[node as usize].entry(start, inverse, limit).map(|time| (node, time));
        if self.nodes.is_empty() {
            return;
        }
        let Some(root) = entry(0, limit) else { return };
        // Each split halves a node's triangles, so the depth stays far below the stack's size.
        let mut stack = [root; 64];
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let (index, time) = stack[top];
            if time > limit {
                continue;
            }
            let node = &self.nodes[index as usize];
            if node.count == 0 {
                let (a, b) = (entry(index + 1, limit), entry(node.first, limit));
                let [near, far] = if b.is_some_and(|b| a.is_none_or(|a| b.1 < a.1)) { [b, a] } else { [a, b] };
                for child in [far, near].into_iter().flatten() {
                    stack[top] = child;
                    top += 1;
                }
            } else {
                let first = node.first as usize;
                for &triangle in &self.order[first..first + node.count as usize] {
                    limit = visit(triangle as usize);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor() -> TriangleTree {
        let cell = |x: f32, y: f32| [[x * 100.0, y * 100.0, 0.0], [x * 100.0 + 100.0, y * 100.0, 0.0], [x * 100.0, y * 100.0 + 100.0, 0.0]];
        let triangles: Vec<[[f32; 3]; 3]> = (0..64).flat_map(|y| (0..64).map(move |x| cell(x as f32, y as f32))).collect();
        TriangleTree::new(&triangles)
    }

    fn visits(tree: &TriangleTree, start: Vector, end: Vector, next_limit: f32) -> Vec<usize> {
        let mut visited = Vec::new();
        tree.along(start, end, 1.0, |triangle| {
            visited.push(triangle);
            next_limit
        });
        visited
    }

    #[test]
    fn a_segment_visits_few_triangles() {
        let tree = floor();
        let drop = visits(&tree, Vector::new(3210.0, 3240.0, 50.0), Vector::new(3210.0, 3240.0, -50.0), 1.0);
        assert!(drop.contains(&(32 * 64 + 32)) && drop.len() < 256, "{} triangles", drop.len());
        let run = visits(&tree, Vector::new(-100.0, 3250.0, 0.5), Vector::new(6500.0, 3250.0, 0.5), 1.0);
        assert!((64..1024).contains(&run.len()), "{} triangles", run.len());
    }

    #[test]
    fn the_nearest_leaf_comes_first_and_the_limit_ends_the_walk() {
        let tree = floor();
        let visited = visits(&tree, Vector::new(-100.0, 3250.0, 0.5), Vector::new(6500.0, 3250.0, 0.5), 0.0);
        assert!((1..=LEAF_TRIANGLES).contains(&visited.len()), "{visited:?}");
        assert!(visited.iter().all(|triangle| triangle % 64 < 10), "{visited:?}");
    }

    #[test]
    fn a_segment_off_the_boxes_visits_nothing() {
        let tree = floor();
        assert!(visits(&tree, Vector::new(-50.0, -50.0, 50.0), Vector::new(-50.0, -50.0, -50.0), 1.0).is_empty());
        assert!(visits(&tree, Vector::new(100.0, 100.0, 2.0), Vector::new(200.0, 100.0, 2.0), 1.0).is_empty());
        assert!(!visits(&tree, Vector::new(150.0, 150.0, 0.0), Vector::new(250.0, 150.0, 0.0), 1.0).is_empty());
        assert!(TriangleTree::new(&[]).nodes.is_empty());
        assert!(visits(&TriangleTree::new(&[]), Vector::ZERO, Vector::new(1.0, 1.0, 1.0), 1.0).is_empty());
    }
}
