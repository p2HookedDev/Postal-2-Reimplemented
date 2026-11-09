use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;

use p2_assets::{BspNode, ConvexVolume, Model, poly_flags};

use crate::MeshPlacement;
use p2_math::Vector;

/// A plane: points `p` with `normal · p >= d` are inside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub normal: Vector,
    pub d: f32,
}

impl Plane {
    fn flipped(self) -> Self {
        Self { normal: self.normal * -1.0, d: -self.d }
    }
}

impl From<[f32; 4]> for Plane {
    fn from([x, y, z, d]: [f32; 4]) -> Self {
        Self { normal: Vector::new(x, y, z), d }
    }
}

const MAX_VOLUMES: usize = 512;

/// Closer than this to a portal's plane (units), the eye sees through all of it: the volume behind is the one in
/// front (the eye-through-polygon planes degenerate there).
const IN_PORTAL: f32 = 1.0;

struct Portal {
    polygon: Vec<Vector>,
    plane: Plane,
    zones: [u8; 2],
}

/// A `PF_AntiPortal` surface: retail builds its occluder from the source brush poly; ours is the convex hull of the
/// surface's BSP fragments, which lies inside that poly (so it never hides more than retail).
struct Antiportal {
    polygon: Vec<Vector>,
    plane: Plane,
    zones: [u8; 2],
    two_sided: bool,
    min: Vector,
    max: Vector,
}

/// An outward plane (the solid behind) with its world vertices.
struct Face {
    plane: Plane,
    vertices: Vec<Vector>,
    /// Per edge (vertex i to i + 1): the faces with vertices at both its ends.
    neighbours: Vec<Vec<usize>>,
}

/// An antiportal actor: its convex volume and the zones its box touches. Seen from outside it hides
/// what lies wholly in its shadow: behind every face the eye is in front of, inside its silhouette.
pub struct AntiportalActor {
    faces: Vec<Face>,
    centre: Vector,
    zones: Vec<u8>,
    min: Vector,
    max: Vector,
}

impl AntiportalActor {
    pub fn new(volume: &ConvexVolume, placement: &MeshPlacement, zones: impl FnOnce(Vector, Vector) -> Vec<u8>) -> Option<Self> {
        let faces: Vec<Vec<Vector>> =
            volume.faces.iter().map(|face| face.vertices.iter().map(|&v| placement.transform_point(v.into())).collect()).collect();
        let points = || faces.iter().flatten();
        let count = points().count();
        if faces.len() < 4 || count == 0 {
            return None;
        }
        let centre = points().fold(Vector::ZERO, |sum, &p| sum + p) * (1.0 / count as f32);
        let (min, max) = bounding_box(points());
        let faces: Vec<(Plane, Vec<Vector>)> = faces
            .into_iter()
            .filter(|vertices| vertices.len() >= 3)
            .map(|vertices| {
                let normal = (0..vertices.len()).fold(Vector::ZERO, |sum, i| {
                    sum + vertices[i].cross(vertices[(i + 1) % vertices.len()])
                }).safe_normal();
                let plane = Plane { normal, d: normal.dot(vertices[0]) };
                (if plane_dot(&plane, centre) > 0.0 { plane.flipped() } else { plane }, vertices)
            })
            .collect();
        let near = |a: Vector, b: Vector| (0..3).all(|k| (a[k] - b[k]).abs() < 0.5);
        let sharing = |a: Vector, b: Vector| -> Vec<usize> {
            (0..faces.len()).filter(|&g| faces[g].1.iter().any(|&v| near(v, a)) && faces[g].1.iter().any(|&v| near(v, b))).collect()
        };
        let neighbours: Vec<Vec<Vec<usize>>> = faces
            .iter()
            .map(|(_, vertices)| (0..vertices.len()).map(|i| sharing(vertices[i], vertices[(i + 1) % vertices.len()])).collect())
            .collect();
        let faces = faces.into_iter().zip(neighbours).map(|((plane, vertices), neighbours)| Face { plane, vertices, neighbours }).collect();
        Some(Self { faces, centre, zones: zones(min, max), min, max })
    }

    /// Appends the occluder's planes to `planes`; `false` (nothing appended) when the eye faces no face.
    fn occluder(&self, eye: Vector, planes: &mut Vec<Plane>) -> bool {
        let front = |face: &Face| plane_dot(&face.plane, eye) > 0.0;
        if !self.faces.iter().any(front) {
            return false;
        }
        planes.extend(self.faces.iter().filter(|face| front(face)).map(|face| face.plane.flipped()));
        for face in self.faces.iter().filter(|face| front(face)) {
            for (i, (&a, neighbours)) in face.vertices.iter().zip(&face.neighbours).enumerate() {
                let b = face.vertices[(i + 1) % face.vertices.len()];
                let behind = neighbours.iter().any(|&g| !front(&self.faces[g]));
                let normal = (a - eye).cross(b - eye).safe_normal();
                if behind && normal != Vector::ZERO {
                    let plane = Plane { normal, d: normal.dot(eye) };
                    planes.push(if plane_dot(&plane, self.centre) >= 0.0 { plane } else { plane.flipped() });
                }
            }
        }
        true
    }
}

#[derive(Clone, Default)]
struct ZoneMembers {
    portals: Vec<usize>,
    antiportals: Vec<usize>,
    actors: Vec<usize>,
}

pub struct ZonePortals {
    portals: Vec<Portal>,
    antiportals: Vec<Antiportal>,
    actors: Vec<AntiportalActor>,
    /// By zone number, each list in ascending index order.
    members: Vec<ZoneMembers>,
}

impl ZonePortals {
    pub fn new(model: &Model, actors: Vec<AntiportalActor>) -> Self {
        let polygon = |node: &BspNode| {
            (0..usize::from(node.num_vertices))
                .map(|i| model.verts.get(node.vert_pool + i).and_then(|vert| model.points.get(vert.point).copied().map(Vector::from)))
                .collect::<Option<Vec<_>>>()
        };
        let flags = |node: &BspNode| model.surfaces.get(node.surface).map_or(0, |surface| surface.poly_flags);
        let portals = model
            .nodes
            .iter()
            .filter(|node| node.zones[0] != node.zones[1] && flags(node) & poly_flags::PORTAL != 0)
            .filter_map(|node| {
                let polygon = polygon(node)?;
                (polygon.len() >= 3).then_some(Portal { polygon, plane: node.plane.into(), zones: node.zones })
            })
            .collect();
        let mut fragments: BTreeMap<usize, (&BspNode, Vec<Vector>)> = BTreeMap::new();
        for node in model.nodes.iter().filter(|node| flags(node) & (poly_flags::ANTI_PORTAL | poly_flags::PORTAL) == poly_flags::ANTI_PORTAL) {
            if let Some(points) = polygon(node) {
                fragments.entry(node.surface).or_insert((node, Vec::new())).1.extend(points);
            }
        }
        let antiportals = fragments
            .into_values()
            .filter_map(|(node, points)| {
                let polygon = hull(&points, node.plane.into());
                let (min, max) = bounding_box(polygon.iter());
                let two_sided = flags(node) & poly_flags::TWO_SIDED != 0;
                (polygon.len() >= 3).then_some(Antiportal { polygon, plane: node.plane.into(), zones: node.zones, two_sided, min, max })
            })
            .collect();
        Self::indexed(portals, antiportals, actors)
    }

    fn indexed(portals: Vec<Portal>, antiportals: Vec<Antiportal>, actors: Vec<AntiportalActor>) -> Self {
        let mut members = vec![ZoneMembers::default(); 256];
        let add = |list: &mut Vec<usize>, index: usize| {
            if list.last() != Some(&index) {
                list.push(index);
            }
        };
        for (index, portal) in portals.iter().enumerate() {
            portal.zones.iter().for_each(|&z| add(&mut members[usize::from(z)].portals, index));
        }
        for (index, anti) in antiportals.iter().enumerate() {
            anti.zones.iter().for_each(|&z| add(&mut members[usize::from(z)].antiportals, index));
        }
        for (index, actor) in actors.iter().enumerate() {
            actor.zones.iter().for_each(|&z| add(&mut members[usize::from(z)].actors, index));
        }
        Self { portals, antiportals, actors, members }
    }

    /// Each portal of a reached zone that the eye faces from that zone's side is clipped by the zone's view volumes; what is
    /// left opens the zone behind. Antiportals hide, in that zone, what lies wholly behind them, portals included (retail
    /// adds them as its front-to-back walk meets them; here as each volume is expanded). Zone 0 (outside the world) sees
    /// everything. `seen` is reused from walk to walk.
    pub fn visible(&self, eye: Vector, zone: u8, view: &[Plane], seen: &mut VisibleZones) {
        let VisibleZones { walked, capped, planes, volumes, occluders, antiportals_used, actors_used, antiportal_sides, portal_sides, queue } =
            seen;
        *walked = false;
        *capped = false;
        if zone == 0 || self.portals.is_empty() {
            return;
        }
        planes.clear();
        planes.extend_from_slice(view);
        volumes.resize_with(256, Vec::new);
        occluders.resize_with(256, Vec::new);
        volumes.iter_mut().chain(occluders.iter_mut()).for_each(Vec::clear);
        volumes[usize::from(zone)].push(0..planes.len());
        antiportals_used.clear();
        antiportals_used.resize(self.antiportals.len(), false);
        actors_used.clear();
        actors_used.resize(self.actors.len(), false);
        antiportal_sides.clear();
        antiportal_sides.extend(self.antiportals.iter().map(|anti| plane_dot(&anti.plane, eye)));
        portal_sides.clear();
        portal_sides.extend(self.portals.iter().map(|portal| plane_dot(&portal.plane, eye)));
        queue.clear();
        queue.push_back((zone, 0));
        let mut opened = 1;
        while let Some((zone, i)) = queue.pop_front() {
            let members = &self.members[usize::from(zone)];
            let here = usize::from(zone);
            for &index in &members.antiportals {
                if antiportals_used[index] {
                    continue;
                }
                let (anti, side) = (&self.antiportals[index], antiportal_sides[index]);
                let near = anti.zones[usize::from(side >= 0.0)];
                if near != zone || side.abs() < IN_PORTAL || (side < 0.0 && !anti.two_sided) {
                    continue;
                }
                if !touches(&planes[volumes[here][i].clone()], anti.min, anti.max) {
                    continue;
                }
                let start = planes.len();
                if through(eye, &anti.polygon, &anti.plane, side, planes) {
                    planes[start..].rotate_right(1);
                    planes[start].d += IN_PORTAL;
                    occluders[here].push(start..planes.len());
                    antiportals_used[index] = true;
                }
            }
            for &index in &members.actors {
                let actor = &self.actors[index];
                if actors_used[index] || !touches(&planes[volumes[here][i].clone()], actor.min, actor.max) {
                    continue;
                }
                actors_used[index] = true;
                let start = planes.len();
                if actor.occluder(eye, planes) {
                    for &z in &actor.zones {
                        occluders[usize::from(z)].push(start..planes.len());
                    }
                }
            }
            for &index in &members.portals {
                let (portal, side) = (&self.portals[index], portal_sides[index]);
                let [back, front] = portal.zones;
                let (near, far) = if side >= 0.0 { (front, back) } else { (back, front) };
                if near != zone {
                    continue;
                }
                if opened >= MAX_VOLUMES {
                    *capped = true;
                    return;
                }
                let hidden = occluders[here]
                    .iter()
                    .any(|o| portal.polygon.iter().all(|&p| planes[o.clone()].iter().all(|plane| plane_dot(plane, p) >= 0.0)));
                if hidden {
                    continue;
                }
                let volume = volumes[here][i].clone();
                let start = planes.len();
                let opens = if side.abs() < IN_PORTAL {
                    planes.extend_from_within(volume);
                    true
                } else {
                    let clipped = clip(&portal.polygon, &planes[volume]);
                    through(eye, &clipped, &portal.plane, side, planes)
                };
                if !opens {
                    continue;
                }
                volumes[usize::from(far)].push(start..planes.len());
                queue.push_back((far, volumes[usize::from(far)].len() - 1));
                opened += 1;
            }
        }
        *walked = true;
    }
}

/// The result of [`ZonePortals::visible`], kept by the caller so its buffers are reused.
#[derive(Default)]
pub struct VisibleZones {
    /// `false`: everything is seen.
    walked: bool,
    capped: bool,
    /// The volumes and occluders are ranges of this.
    planes: Vec<Plane>,
    /// By zone number.
    volumes: Vec<Vec<Range<usize>>>,
    occluders: Vec<Vec<Range<usize>>>,
    antiportals_used: Vec<bool>,
    actors_used: Vec<bool>,
    antiportal_sides: Vec<f32>,
    portal_sides: Vec<f32>,
    queue: VecDeque<(u8, usize)>,
}

impl VisibleZones {
    /// The walk passed its cap, so everything counts as seen: incomplete volumes would hide what is in view.
    pub fn capped(&self) -> bool {
        self.capped
    }

    pub fn reached(&self, zone: u8) -> bool {
        !self.walked || self.volumes.get(usize::from(zone)).is_some_and(|volume| !volume.is_empty())
    }

    pub fn sees(&self, zone: u8, min: Vector, max: Vector) -> bool {
        if !self.walked {
            return true;
        }
        let zone = usize::from(zone);
        self.volumes[zone].iter().any(|volume| touches(&self.planes[volume.clone()], min, max))
            && !self.occluders[zone].iter().any(|occluder| {
                self.planes[occluder.clone()].iter().all(|plane| {
                    let corner = [0, 1, 2].map(|k| if plane.normal[k] >= 0.0 { min[k] } else { max[k] }).into();
                    plane_dot(plane, corner) >= 0.0
                })
            })
    }
}

fn touches(volume: &[Plane], min: Vector, max: Vector) -> bool {
    volume.iter().all(|plane| {
        let corner = [0, 1, 2].map(|k| if plane.normal[k] >= 0.0 { max[k] } else { min[k] }).into();
        plane_dot(plane, corner) >= 0.0
    })
}

fn plane_dot(plane: &Plane, p: Vector) -> f32 {
    plane.normal.dot(p) - plane.d
}

fn bounding_box<'a>(points: impl Iterator<Item = &'a Vector>) -> (Vector, Vector) {
    let (min, max) = points.fold(([f32::MAX; 3], [f32::MIN; 3]), |(min, max), p| ([0, 1, 2].map(|k| min[k].min(p[k])), [0, 1, 2].map(|k| max[k].max(p[k]))));
    (min.into(), max.into())
}

fn hull(points: &[Vector], plane: Plane) -> Vec<Vector> {
    let Vector { x, y, z } = plane.normal;
    let normal = [x.abs(), y.abs(), z.abs()];
    let drop = if normal[0] >= normal[1] && normal[0] >= normal[2] { 0 } else if normal[1] >= normal[2] { 1 } else { 2 };
    let (u, v) = ((drop + 1) % 3, (drop + 2) % 3);
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a[u].total_cmp(&b[u]).then(a[v].total_cmp(&b[v])));
    sorted.dedup_by(|a, b| (a[u] - b[u]).abs() < 0.01 && (a[v] - b[v]).abs() < 0.01);
    if sorted.len() < 3 {
        return sorted;
    }
    let turn = |o: Vector, a: Vector, b: Vector| (a[u] - o[u]) * (b[v] - o[v]) - (a[v] - o[v]) * (b[u] - o[u]);
    let mut chain: Vec<Vector> = Vec::new();
    for pass in [sorted.clone(), sorted.into_iter().rev().collect()] {
        let start = chain.len();
        for p in pass {
            while chain.len() >= start + 2 && turn(chain[chain.len() - 2], chain[chain.len() - 1], p) <= 0.0 {
                chain.pop();
            }
            chain.push(p);
        }
        chain.pop();
    }
    chain
}

fn clip(polygon: &[Vector], planes: &[Plane]) -> Vec<Vector> {
    let mut out = polygon.to_vec();
    for plane in planes {
        if out.len() < 3 {
            break;
        }
        let input = std::mem::take(&mut out);
        for (i, &a) in input.iter().enumerate() {
            let b = input[(i + 1) % input.len()];
            let (da, db) = (plane_dot(plane, a), plane_dot(plane, b));
            if da >= 0.0 {
                out.push(a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                out.push(a.lerp(b, da / (da - db)));
            }
        }
    }
    out
}

/// Appends the planes of the volume seen through `polygon` to `planes`; `false` (nothing appended) for a degenerate polygon.
fn through(eye: Vector, polygon: &[Vector], portal: &Plane, side: f32, planes: &mut Vec<Plane>) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let center = polygon.iter().fold(Vector::ZERO, |sum, &p| sum + p) * (1.0 / polygon.len() as f32);
    planes.extend(polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .filter_map(|(&a, &b)| {
            let normal = (a - eye).cross(b - eye).safe_normal();
            (normal != Vector::ZERO).then(|| {
                let plane = Plane { normal, d: normal.dot(eye) };
                if normal.dot(center) >= plane.d { plane } else { plane.flipped() }
            })
        }));
    let s = -side.signum();
    planes.push(Plane { normal: portal.normal * s, d: portal.d * s });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn door() -> Portal {
        let polygon = [[0.0, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 1.0, 1.0], [0.0, -1.0, 1.0]].map(Vector::from).to_vec();
        Portal { polygon, plane: [1.0, 0.0, 0.0, 0.0].into(), zones: [1, 2] }
    }

    fn doorway() -> ZonePortals {
        ZonePortals::indexed(vec![door()], Vec::new(), Vec::new())
    }

    fn walk(portals: &ZonePortals, eye: [f32; 3], zone: u8, view: &[Plane]) -> VisibleZones {
        let mut seen = VisibleZones::default();
        portals.visible(eye.into(), zone, view, &mut seen);
        seen
    }

    fn cube() -> ConvexVolume {
        let face = |axis: usize, sign: f32| {
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            let vertices = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]]
                .map(|[a, b]| {
                    let mut p = [0.0; 3];
                    (p[axis], p[u], p[v]) = (sign, a, b);
                    p
                })
                .to_vec();
            let mut plane = [0.0; 4];
            (plane[axis], plane[3]) = (sign, 1.0);
            p2_assets::ConvexFace { plane, vertices }
        };
        let faces = (0..3).flat_map(|axis| [face(axis, -1.0), face(axis, 1.0)]).collect();
        ConvexVolume { faces, edges: Vec::new() }
    }

    #[test]
    fn an_antiportal_actor_hides_its_shadow_in_its_zones() {
        let placement = MeshPlacement { location: [-2.5, 0.0, 0.0].into(), rotation: [0; 3].into(), scale: [2.0; 3].into(), pre_pivot: [0.0; 3].into() };
        let actor = AntiportalActor::new(&cube(), &placement, |_, _| vec![1]).unwrap();
        let portals = ZonePortals::indexed(vec![door()], Vec::new(), vec![actor]);
        let seen = walk(&portals, [-10.0, 0.0, 0.0], 1, &[]);
        assert!(!seen.reached(2));
        assert!(!seen.sees(1, [-0.4, -0.5, -0.5].into(), [-0.2, 0.5, 0.5].into()));
        assert!(seen.sees(1, [-0.4, 9.0, -0.5].into(), [-0.2, 10.0, 0.5].into()));
        assert!(seen.sees(1, [-6.0, -0.5, -0.5].into(), [-0.2, 0.5, 0.5].into()));
        assert!(walk(&portals, [-2.5, 0.0, 0.0], 1, &[]).reached(2));
    }

    fn wall(y: f32, half: f32) -> Antiportal {
        let corners = [[-half, -half], [half, -half], [half, half], [-half, half]];
        let polygon: Vec<Vector> = corners.iter().map(|&[dy, dz]| Vector::new(-2.0, y + dy, dz)).collect();
        let plane = Plane::from([-1.0, 0.0, 0.0, 2.0]);
        Antiportal { min: [-2.0, y - half, -half].into(), max: [-2.0, y + half, half].into(), polygon: hull(&polygon, plane), plane, zones: [1, 1], two_sided: false }
    }

    #[test]
    fn an_antiportal_hides_what_lies_wholly_behind_it() {
        let portals = ZonePortals::indexed(vec![door()], vec![wall(0.0, 10.0)], Vec::new());
        let seen = walk(&portals, [-5.0, 0.0, 0.0], 1, &[]);
        assert!(!seen.reached(2));
        assert!(!seen.sees(1, [-0.9, -1.0, -1.0].into(), [-0.5, 1.0, 1.0].into()));
        assert!(seen.sees(1, [-3.0, -1.0, -1.0].into(), [-0.5, 1.0, 1.0].into()));
        assert!(seen.sees(1, [-2.0, -1.0, -1.0].into(), [-2.0, 1.0, 1.0].into()));
        assert!(seen.sees(1, [-0.9, 20.0, -1.0].into(), [-0.5, 21.0, 1.0].into()));
        let seen = walk(&portals, [-1.0, 0.0, 0.0], 1, &[]);
        assert!(seen.sees(1, [-4.0, -1.0, -1.0].into(), [-3.5, 1.0, 1.0].into()));
    }

    #[test]
    fn an_antiportal_beside_a_portal_leaves_it_open() {
        let portals = ZonePortals::indexed(vec![door()], vec![wall(5.0, 2.0)], Vec::new());
        assert!(walk(&portals, [-5.0, 0.0, 0.0], 1, &[]).reached(2));
    }

    #[test]
    fn hull_of_fragments() {
        let points = [[0.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 2.0, 2.0], [0.0, 0.0, 0.0], [0.0, 2.0, 2.0], [0.0, 0.0, 2.0], [0.0, 1.0, 1.0]];
        assert_eq!(hull(&points.map(Vector::from), [1.0, 0.0, 0.0, 0.0].into()).len(), 4);
    }

    #[test]
    fn a_portal_opens_the_zone_behind_it_narrowed() {
        let seen = walk(&doorway(), [-5.0, 0.0, 0.0], 1, &[]);
        assert!(seen.reached(2) && !seen.reached(3));
        assert!(seen.sees(2, [10.0, -0.5, -0.5].into(), [11.0, 0.5, 0.5].into()));
        assert!(!seen.sees(2, [10.0, 50.0, -0.5].into(), [11.0, 51.0, 0.5].into()));
        assert!(seen.sees(1, [-100.0, 50.0, 0.0].into(), [-99.0, 51.0, 1.0].into()));
    }

    #[test]
    fn a_portal_out_of_view_opens_nothing() {
        let seen = walk(&doorway(), [-5.0, 0.0, 0.0], 1, &[Plane::from([-1.0, 0.0, 0.0, 6.0])]);
        assert!(!seen.reached(2));
        assert!(!seen.sees(2, [10.0, -0.5, -0.5].into(), [11.0, 0.5, 0.5].into()));
    }

    #[test]
    fn a_walk_past_its_cap_sees_all() {
        let portals = ZonePortals::indexed((0..=MAX_VOLUMES).map(|_| door()).collect(), Vec::new(), Vec::new());
        let seen = walk(&portals, [-5.0, 0.0, 0.0], 1, &[]);
        assert!(seen.capped());
        assert!(seen.sees(2, [10.0, 50.0, -0.5].into(), [11.0, 51.0, 0.5].into()));
    }

    #[test]
    fn outside_the_world_or_in_the_portal_sees_all() {
        assert!(walk(&doorway(), [-5.0, 0.0, 0.0], 0, &[]).sees(7, [0.0; 3].into(), [1.0; 3].into()));
        let seen = walk(&doorway(), [-0.5, 0.0, 0.0], 1, &[]);
        assert!(seen.sees(2, [10.0, 50.0, -0.5].into(), [11.0, 51.0, 0.5].into()));
    }
}
