use std::cmp::Reverse;
use std::collections::BinaryHeap;

use p2_math::{Rng, Vector};
use rustc_hash::FxHashMap;
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::Prop;
use crate::interp::{float, int};
use crate::natives::{object_arg, vec3};
use crate::physics::{MAX_STEP_HEIGHT, MIN_FLOOR_Z, MoveHit, Sweep, SweepAgainst};
use crate::{ClassNames, Error, Kind, LoaderRef, ObjectId, ObjectStore, Value, Vm};

const UNVISITED: i32 = 10_000_000;
const ANCHOR_DISTANCE_SQUARED: i32 = 1_440_000;
const ANCHOR_CELL: f32 = 1200.0;
/// The anchor distance plus a margin for the rounding of the cell bounds.
const ANCHOR_REACH: f32 = 1201.0;
/// Within this the rounding stays far below the margin; a node or a query beyond it is scanned in full.
const ANCHOR_GRID_LIMIT: f32 = 1_048_576.0;
const REACH_DISTANCE_SQUARED: f32 = 1_000_000.0;
const WALK_STEPS: usize = 100;
pub(crate) const WALK_MIN_MOVE: f32 = 4.1;
const WALK_MIN_LAST_MOVE: f32 = 8.0;
const FLY_STEP: f32 = 200.0;
const FLOOR_DROP: f32 = MAX_STEP_HEIGHT + 2.0;
/// How often one reach test may pass between its air and water halves. Retail has no limit and would run out of stack
/// long after this many, so the answers are the same wherever retail gives one.
const HAND_OFFS: u32 = 64;
/// A swimmer that leaves the water tries to climb out only toward a goal lower than this above it.
const SWIM_EXIT_HEIGHT: f32 = 78.0 + MAX_STEP_HEIGHT;
const SKIP_ANCHOR_DISTANCE: f32 = 1200.0;
const SKIP_ANCHOR_SHARE: f32 = 0.85;
const R_FORCED: i32 = 256;
const ROUTE_CACHE: usize = 16;

#[derive(Debug, Clone)]
struct Spec {
    end: usize,
    distance: i32,
    radius: i32,
    height: i32,
    flags: i32,
    max_landing: i32,
}

#[derive(Debug, Clone)]
struct Node {
    actor: ObjectId,
    location: Vector,
    radius: f32,
    height: f32,
    specs: Vec<Spec>,
}

#[derive(Debug, Default)]
struct Search {
    query: u32,
    stamps: Vec<u32>,
    weight: Vec<i32>,
    previous: Vec<Option<usize>>,
    blocked: Vec<bool>,
    cost: Vec<i32>,
}

impl Search {
    fn start(&mut self, count: usize) {
        self.stamps.resize(count, 0);
        self.weight.resize(count, UNVISITED);
        self.previous.resize(count, None);
        self.blocked.resize(count, false);
        self.cost.resize(count, 0);
        self.query = self.query.wrapping_add(1);
        if self.query == 0 {
            self.stamps.fill(0);
            self.query = 1;
        }
    }

    fn reach(&mut self, store: &ObjectStore, node: usize, actor: ObjectId) {
        if self.stamps[node] != self.query {
            self.stamps[node] = self.query;
            self.weight[node] = UNVISITED;
            self.previous[node] = None;
            self.blocked[node] = store.flag_of(actor, Prop::bBlocked);
            self.cost[node] = store.get(actor, Prop::ExtraCost).as_ref().map_or(0, int);
        }
    }
}

struct Searched {
    best: Option<usize>,
    best_weight: f32,
    search: Search,
}

#[derive(Debug, Default)]
pub(crate) struct Graph {
    nodes: Vec<Node>,
    index: FxHashMap<ObjectId, usize>,
    cells: FxHashMap<(i32, i32), Vec<usize>>,
    wide: Vec<usize>,
    search: Search,
}

fn anchor_cell(v: f32) -> i32 {
    (v / ANCHOR_CELL).floor() as i32
}

fn gridded(location: Vector) -> bool {
    location.x.abs() < ANCHOR_GRID_LIMIT && location.y.abs() < ANCHOR_GRID_LIMIT && location.z.is_finite()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Evaluator {
    EndPoint(Option<usize>),
    Random,
}

#[derive(Clone, Copy)]
struct FoundRoute<'a> {
    best: usize,
    weight: &'a [i32],
    previous: &'a [Option<usize>],
    start_distance: f32,
    end_distance: f32,
}

impl Graph {
    fn nodes_near(&self, store: &ObjectStore, location: Vector) -> Vec<usize> {
        let anchor = |i: usize| {
            let node = &self.nodes[i];
            let offset = location - node.location;
            let squared = offset.dot(offset) as i32;
            (squared < ANCHOR_DISTANCE_SQUARED && !store.flag_of(node.actor, Prop::bBlocked)).then_some((squared, i))
        };
        let mut near: Vec<(i32, usize)> = if gridded(location) {
            let span = |v: f32| anchor_cell(v - ANCHOR_REACH)..=anchor_cell(v + ANCHOR_REACH);
            let ys = span(location.y);
            span(location.x)
                .flat_map(|x| ys.clone().map(move |y| (x, y)))
                .filter_map(|cell| self.cells.get(&cell))
                .flatten()
                .chain(&self.wide)
                .filter_map(|&i| anchor(i))
                .collect()
        } else {
            (0..self.nodes.len()).filter_map(anchor).collect()
        };
        near.sort();
        near.into_iter().map(|(_, i)| i).collect()
    }

    fn breadth_path_to(&mut self, store: &ObjectStore, rng: &mut Rng, pawn: ObjectId, start: usize, start_weight: i32, evaluator: Evaluator) -> Searched {
        let can_crouch = store.flag_of(pawn, Prop::bCanCrouch);
        let (radius, height) = match can_crouch {
            true => (store.float_of(pawn, Prop::CrouchRadius), store.float_of(pawn, Prop::CrouchHeight)),
            false => store.collision_size(pawn),
        };
        let (radius, height) = (radius as i32, height as i32);
        let max_fall = store.float_of(pawn, Prop::MaxFallSpeed) as i32;
        let crouch_cost = 1.1 / store.float_of(pawn, Prop::WalkingPct);
        let default_height = store.object(pawn).class.and_then(|c| store.default_value(c, Prop::CollisionHeight)).as_ref().map_or(0.0, float);
        let flags = move_flags(store, pawn);
        let mut search = std::mem::take(&mut self.search);
        search.start(self.nodes.len());
        search.reach(store, start, self.nodes[start].actor);
        search.weight[start] = start_weight;
        let mut queue = BinaryHeap::from([Reverse((start_weight, start))]);
        let (mut best, mut best_weight) = (None, 0.0_f32);
        let mut visits = 0;
        while let Some(Reverse((w, current))) = queue.pop() {
            if w != search.weight[current] {
                continue;
            }
            let value = match evaluator {
                Evaluator::EndPoint(end) => if Some(current) == end { 2.0 } else { 0.0 },
                Evaluator::Random => rng.frand(),
            };
            if value > best_weight && !search.blocked[current] {
                best_weight = value;
                best = Some(current);
            }
            if best_weight >= 1.0 {
                break;
            }
            let over = visits > 250;
            visits += 1;
            if over {
                if best_weight > 0.0 {
                    break;
                }
                visits = 200;
            }
            if search.blocked[current] || hurt_by_volume(store, pawn, self.nodes[current].actor) {
                continue;
            }
            for spec in &self.nodes[current].specs {
                let supported = spec.radius >= radius && spec.height >= height && spec.flags & flags == spec.flags && spec.max_landing <= max_fall;
                if !supported || spec.end == current {
                    continue;
                }
                search.reach(store, spec.end, self.nodes[spec.end].actor);
                let edge = if (spec.height as f32) < default_height {
                    (spec.distance as f32 * crouch_cost + search.cost[spec.end] as f32) as i32
                } else {
                    search.cost[spec.end] + spec.distance
                };
                let next = search.weight[current].saturating_add(edge);
                if next < search.weight[spec.end] {
                    search.weight[spec.end] = next;
                    search.previous[spec.end] = Some(current);
                    queue.push(Reverse((next, spec.end)));
                }
            }
        }
        Searched { best, best_weight, search }
    }

    fn start_route(&self, store: &mut ObjectStore, controller: ObjectId, found: &FoundRoute) -> (Vec<usize>, Vec<(ObjectId, Vector)>) {
        let FoundRoute { best, weight, previous, end_distance, .. } = *found;
        let mut route = vec![best];
        while let Some(&Some(before)) = previous.get(route[route.len() - 1]) {
            if route.len() > previous.len() {
                break;
            }
            route.push(before);
        }
        route.reverse();
        let nodes: Vec<(ObjectId, Vector)> = route.iter().map(|&n| (self.nodes[n].actor, self.nodes[n].location)).collect();
        store.set(controller, Prop::RouteGoal, Value::Object(Some(nodes[nodes.len() - 1].0)));
        store.set(controller, Prop::RouteDist, Value::Float(weight[best] as f32 + end_distance));
        (route, nodes)
    }

    fn finish_route(&self, store: &mut ObjectStore, controller: ObjectId, pawn: ObjectId, route: &[usize], nodes: &[(ObjectId, Vector)], first: usize) {
        let cache: Vec<Value> = (0..ROUTE_CACHE).map(|i| Value::Object(nodes.get(first + i).map(|n| n.0))).collect();
        store.set(controller, Prop::RouteCache, Value::Array(cache));
        if let (Some(&a), Some(&b)) = (route.get(first), route.get(first + 1))
            && let Some(spec) = self.nodes[a].specs.iter().find(|s| s.end == b)
        {
            let radius = spec.radius as f32;
            store.set(pawn, Prop::NextPathRadius, Value::Float(radius));
        }
    }
}

fn read_graph(store: &ObjectStore, level: Option<ObjectId>) -> Graph {
    let mut graph = Graph::default();
    let mut current = level.and_then(|level| store.object_of(level, Prop::NavigationPointList));
    let mut actors = Vec::new();
    while let Some(nav) = current {
        if graph.index.contains_key(&nav) {
            break;
        }
        graph.index.insert(nav, actors.len());
        actors.push(nav);
        current = store.object_of(nav, Prop::nextNavigationPoint);
    }
    for &actor in &actors {
        let specs = match store.get(actor, Prop::PathList) {
            Some(Value::Array(list)) => list
                .iter()
                .filter_map(|v| if let &Value::Object(Some(spec)) = v { Some(spec) } else { None })
                .filter_map(|spec| {
                    let end = *graph.index.get(&store.object_of(spec, Prop::End)?)?;
                    let number = |name: &str| store.get(spec, name).as_ref().map_or(0, int);
                    Some(Spec {
                        end,
                        distance: number("Distance"),
                        radius: number("CollisionRadius"),
                        height: number("CollisionHeight"),
                        flags: number("reachFlags"),
                        max_landing: number("MaxLandingVelocity"),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        graph.nodes.push(Node {
            actor,
            location: store.vector(actor, Prop::Location),
            radius: store.float_of(actor, Prop::CollisionRadius),
            height: store.float_of(actor, Prop::CollisionHeight),
            specs,
        });
    }
    for (i, node) in graph.nodes.iter().enumerate() {
        if gridded(node.location) {
            graph.cells.entry((anchor_cell(node.location.x), anchor_cell(node.location.y))).or_default().push(i);
        } else {
            graph.wide.push(i);
        }
    }
    graph
}

fn move_flags(store: &ObjectStore, pawn: ObjectId) -> i32 {
    let controller = store.object_of(pawn, Prop::Controller);
    let pawn_flag = |name: Prop| i32::from(store.flag_of(pawn, name));
    let controller_flag = |name: Prop| controller.map_or(0, |c| i32::from(store.flag_of(c, name)));
    pawn_flag(Prop::bCanWalk)
        | pawn_flag(Prop::bCanFly) << 1
        | pawn_flag(Prop::bCanSwim) << 2
        | pawn_flag(Prop::bCanJump) << 3
        | controller_flag(Prop::bCanOpenDoors) << 4
        | controller_flag(Prop::bCanDoSpecial) << 5
        | pawn_flag(Prop::bCanClimbLadders) << 6
        | controller_flag(Prop::bIsPlayer) << 7
        | R_FORCED
}

fn build_node_lists(store: &mut ObjectStore, class_names: &ClassNames, game: ObjectId, homes: bool) {
    let (path_node, home_node) = (class_names.path_node, class_names.home_node);
    let nodes: Vec<ObjectId> = store
        .actor_ids
        .iter()
        .copied()
        .filter(|&o| store.is_a(o, path_node) && !store.is_deleted(o) && store.is_a(o, home_node) == homes)
        .collect();
    let mut lists: Vec<Vec<ObjectId>> = Vec::new();
    for node in nodes {
        let tag = store.get(node, Prop::Tag);
        let found = lists.iter().position(|list| !homes || store.get(list[0], Prop::Tag) == tag);
        match found {
            Some(list) => lists[list].push(node),
            None => lists.push(vec![node]),
        }
    }
    let mut entries = Vec::new();
    for list in &lists {
        for (i, &node) in list.iter().enumerate() {
            store.set(node, Prop::NextNode, Value::Object(Some(list[(i + 1) % list.len()])));
        }
        entries.push(Value::Struct(vec![Value::Object(Some(list[0])), Value::Int(list.len() as i32)]));
    }
    if homes {
        store.set(game, Prop::HomeNodeLists, Value::Array(entries));
    } else if let Some(entry) = entries.pop() {
        store.set(game, Prop::PathNodeList, entry);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineOfSight {
    Unknown,
    Known,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchorCheck {
    Checked,
    Skipped,
}

/// `actor` touches a pain volume that hurts `pawn`: one whose damage is not the pawn's `ReducedDamageType`.
fn hurt_by_volume(store: &ObjectStore, pawn: ObjectId, actor: ObjectId) -> bool {
    let spared = store.object_of(pawn, "ReducedDamageType");
    store.object_list(actor, Prop::Touching).into_iter().any(|volume| {
        store.is_a(volume, Kind::PhysicsVolume)
            && store.flag_of(volume, "bPainCausing")
            && store.object_of(volume, "DamageType") != spared
            && store.float_of(volume, "DamagePerSec") > 0.0
    })
}

impl<L: LoaderRef> Vm<L> {
    fn graph(&mut self) -> &Graph {
        if self.nav_graph.is_none() {
            let graph = read_graph(&self.store, self.level);
            self.nav_graph = Some(graph);
        }
        self.nav_graph.get_or_insert_with(Graph::default)
    }

    pub(crate) fn nav_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(None) };
        Ok(Some(match native {
            Native::FindPathToward => {
                let Some(goal) = object_arg(args, 0) else {
                    self.diagnostics.log.push(format!("Warning: No goal for FindPathToward by {}", self.store.object(this).path));
                    return Ok(Some(Value::Object(None)));
                };
                Value::Object(self.find_path(this, Some(goal), Vector::ZERO)?)
            }
            Native::FindPathTo => Value::Object(self.find_path(this, None, vec3(args.first()))?),
            Native::FindRandomDest => {
                let Some(pawn) = self.store.object_of(this, Prop::Pawn) else { return Ok(Some(Value::Object(None))) };
                let weight = self.find_path_toward(this, pawn, None, Vector::ZERO, Evaluator::Random)?;
                let goal = self.store.object_of(this, Prop::RouteGoal).filter(|&g| weight > 0.0 && self.store.is_a(g, Kind::NavigationPoint));
                Value::Object(goal)
            }
            Native::actorReachable => {
                let reachable = match (self.store.object_of(this, Prop::Pawn), object_arg(args, 0)) {
                    (Some(pawn), Some(target)) => self.actor_reachable(pawn, target, LineOfSight::Unknown, AnchorCheck::Checked),
                    _ => false,
                };
                Value::Bool(reachable)
            }
            Native::pointReachable => {
                let reachable = match self.store.object_of(this, Prop::Pawn) {
                    Some(pawn) => self.point_reachable(pawn, vec3(args.first()), false),
                    None => false,
                };
                Value::Bool(reachable)
            }
            Native::ClearPaths => return Ok(None),
            Native::BuildPathNodeLists | Native::BuildHomeNodeLists => {
                build_node_lists(&mut self.store, &self.script.lookups.class_names, this, native == Native::BuildHomeNodeLists);
                return Ok(None);
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }))
    }

    pub(crate) fn find_path(&mut self, controller: ObjectId, goal: Option<ObjectId>, location: Vector) -> Result<Option<ObjectId>, Error> {
        let Some(pawn) = self.store.object_of(controller, Prop::Pawn) else {
            self.diagnostics.log.push(format!("Warning: No pawn for FindPath by {}", self.store.object(controller).path));
            return Ok(None);
        };
        self.store.set(controller, Prop::bPreparingMove, Value::Bool(false));
        let weight = self.find_path_toward(controller, pawn, goal, location, Evaluator::EndPoint(None))?;
        if weight <= 0.0 {
            return Ok(None);
        }
        Ok(match self.store.get(controller, Prop::RouteCache) {
            Some(Value::Array(cache)) => match cache.first() {
                Some(&Value::Object(next)) => next,
                _ => None,
            },
            _ => None,
        })
    }

    #[expect(clippy::unnecessary_wraps, reason = "its callers return it as a native's result")]
    fn find_path_toward(&mut self, controller: ObjectId, pawn: ObjectId, goal: Option<ObjectId>, goal_location: Vector, evaluator: Evaluator) -> Result<f32, Error> {
        if self.graph().nodes.is_empty() {
            return Ok(0.0);
        }
        self.store.set(pawn, Prop::NextPathRadius, Value::Float(0.0));
        let specified_end = matches!(evaluator, Evaluator::EndPoint(_));
        let mut goal_location = goal_location;
        let mut end_anchor = None;
        let mut end_distance = 0.0;
        if let Some(goal) = goal {
            goal_location = self.store.vector(goal, Prop::Location);
            if self.store.physics_of(pawn) == Some(EPhysics::Walking) {
                let (radius, height) = self.store.collision_size(pawn);
                let hit = self.sweep(pawn, Sweep { start: goal_location, delta: [0.0, 0.0, -1000.0].into(), radius, height }, SweepAgainst::WORLD, None);
                goal_location[2] -= 1000.0 * hit.time;
            }
            if let Some(&node) = self.graph().index.get(&goal) {
                end_anchor = Some(node);
            } else if self.store.is_a(goal, Kind::Pawn) && self.valid_anchor(goal) {
                let anchor = self.store.object_of(goal, Prop::Anchor).and_then(|a| self.graph().index.get(&a).copied());
                if let Some(anchor) = anchor {
                    end_anchor = Some(anchor);
                    end_distance = (self.graph().nodes[anchor].location - goal_location).size();
                }
            }
        }
        if !self.valid_anchor(pawn) {
            self.store.set(pawn, Prop::Anchor, Value::Object(None));
        }
        let mut anchor = self.store.object_of(pawn, Prop::Anchor).and_then(|a| self.graph().index.get(&a).copied());
        let mut start_distance = 0.0;
        if anchor.is_none() || (end_anchor.is_none() && specified_end) {
            let location = self.store.vector(pawn, Prop::Location);
            if anchor.is_none() {
                let Some(found) = self.find_start_anchor(pawn, location) else { return Ok(0.0) };
                let node = &self.graph().nodes[found];
                let (actor, node_location, node_radius) = (node.actor, node.location, node.radius);
                self.store.set(pawn, Prop::Anchor, Value::Object(Some(actor)));
                start_distance = (node_location - location).size();
                if start_distance <= node_radius + self.store.float_of(pawn, Prop::CollisionRadius) {
                    start_distance = 0.0;
                }
                anchor = Some(found);
            }
            if end_anchor.is_none() && specified_end {
                let Some(found) = self.find_end_anchor(pawn, goal, goal_location) else { return Ok(0.0) };
                end_distance = (self.graph().nodes[found].location - goal_location).size();
                end_anchor = Some(found);
            }
            let Some(start) = anchor else { return Ok(0.0) };
            if end_anchor == Some(start) && specified_end {
                return Ok(self.same_anchor_route(controller, pawn, start, goal, goal_location));
            }
        }
        let Some(start) = anchor else { return Ok(0.0) };
        let evaluator = match evaluator {
            Evaluator::EndPoint(_) => Evaluator::EndPoint(end_anchor),
            random => random,
        };
        let Searched { best, best_weight, search } = self.nav_graph.get_or_insert_with(Graph::default).breadth_path_to(&self.store, &mut self.rng, pawn, start, start_distance as i32, evaluator);
        if let Some(best) = best {
            self.set_route_cache(controller, pawn, &FoundRoute { best, weight: &search.weight, previous: &search.previous, start_distance, end_distance });
        }
        if let Some(graph) = &mut self.nav_graph {
            graph.search = search;
        }
        Ok(best_weight)
    }

    fn same_anchor_route(&mut self, controller: ObjectId, pawn: ObjectId, anchor: usize, goal: Option<ObjectId>, goal_location: Vector) -> f32 {
        let (node_actor, node_location) = (self.graph().nodes[anchor].actor, self.graph().nodes[anchor].location);
        let location = self.store.vector(pawn, Prop::Location);
        let first = if self.reached_destination(pawn, node_location - location, goal) {
            if goal.is_none() {
                return 0.0;
            }
            goal
        } else {
            let to_goal = (goal_location - node_location).safe_normal();
            let to_pawn = (location - node_location).safe_normal();
            if to_goal.dot(to_pawn) > 0.9 { goal } else { Some(node_actor) }
        };
        let mut cache = vec![Value::Object(None); ROUTE_CACHE];
        cache[0] = Value::Object(first);
        self.store.set(controller, Prop::RouteCache, Value::Array(cache));
        (goal_location - location).size()
    }

    fn find_start_anchor(&mut self, pawn: ObjectId, location: Vector) -> Option<usize> {
        let height = self.store.float_of(pawn, Prop::CollisionHeight);
        for candidate in self.nav_graph.as_ref().map_or_else(Vec::new, |graph| graph.nodes_near(&self.store, location)) {
            let node = &self.graph().nodes[candidate];
            let (actor, node_location, node_height) = (node.actor, node.location, node.height);
            let visible = self.trace_world(node_location, location, Vector::ZERO).is_none()
                || self.trace_world(Vector::new(node_location.x, node_location.y, node_location.z + node_height), Vector::new(location.x, location.y, location.z + height), Vector::ZERO).is_none();
            if visible && self.actor_reachable(pawn, actor, LineOfSight::Known, AnchorCheck::Skipped) {
                return Some(candidate);
            }
        }
        None
    }

    fn find_end_anchor(&mut self, pawn: ObjectId, goal: Option<ObjectId>, goal_location: Vector) -> Option<usize> {
        let height = self.store.float_of(pawn, Prop::CollisionHeight);
        for candidate in self.nav_graph.as_ref().map_or_else(Vec::new, |graph| graph.nodes_near(&self.store, goal_location)) {
            let node = &self.graph().nodes[candidate];
            let (node_location, node_height) = (node.location, node.height);
            let goal_top = [goal_location[0], goal_location[1], goal_location[2] + goal.map_or(0.0, |g| self.store.float_of(g, Prop::CollisionHeight))];
            let visible = self.trace_world(goal_location, node_location, Vector::ZERO).is_none()
                || self.trace_world(goal_top.into(), Vector::new(node_location.x, node_location.y, node_location.z + node_height), Vector::ZERO).is_none();
            if !visible {
                continue;
            }
            let standing = [node_location.x, node_location.y, node_location.z + height - node_height];
            let reachable = match goal {
                Some(goal) => self.walk_reachable(pawn, standing.into(), self.store.vector(goal, Prop::Location), Some(goal)),
                None => self.walk_reachable(pawn, standing.into(), goal_location, None),
            };
            if reachable {
                return Some(candidate);
            }
        }
        None
    }

    fn valid_anchor(&self, pawn: ObjectId) -> bool {
        let Some(anchor) = self.store.object_of(pawn, Prop::Anchor) else { return false };
        !self.store.flag_of(anchor, Prop::bBlocked)
            && self.reached_destination(pawn, self.store.vector(anchor, Prop::Location) - self.store.vector(pawn, Prop::Location), Some(anchor))
    }

    fn set_route_cache(&mut self, controller: ObjectId, pawn: ObjectId, found: &FoundRoute) {
        let (route, nodes) = self.nav_graph.get_or_insert_with(Graph::default).start_route(&mut self.store, controller, found);
        let mut first = 0;
        if let Some(&(next, next_location)) = nodes.get(1) {
            first = 1;
            if found.start_distance > 0.0 {
                let location = self.store.vector(pawn, Prop::Location);
                let to_pawn = (location - next_location).size();
                let along = (nodes[0].1 - next_location).size();
                let skip = to_pawn < SKIP_ANCHOR_DISTANCE
                    && to_pawn < (along + found.start_distance) * SKIP_ANCHOR_SHARE
                    && self.trace_world(next_location, location, Vector::ZERO).is_none()
                    && self.actor_reachable(pawn, next, LineOfSight::Known, AnchorCheck::Skipped);
                first = usize::from(skip);
            }
        }
        self.nav_graph.get_or_insert_with(Graph::default).finish_route(&mut self.store, controller, pawn, &route, &nodes, first);
    }

    pub(crate) fn actor_reachable(&mut self, pawn: ObjectId, target: ObjectId, sight: LineOfSight, anchor: AnchorCheck) -> bool {
        if self.store.is_deleted(target) {
            return false;
        }
        let location = self.store.vector(pawn, Prop::Location);
        let there = self.store.vector(target, Prop::Location);
        if anchor == AnchorCheck::Checked && self.store.object_of(pawn, Prop::Anchor) == Some(target) && self.valid_anchor(pawn) {
            return true;
        }
        let offset = there - location;
        let squared = offset.dot(offset);
        if squared > REACH_DISTANCE_SQUARED || hurt_by_volume(&self.store, pawn, target) {
            return false;
        }
        let in_water = self.store.object_of(target, Prop::PhysicsVolume).is_some_and(|volume| self.store.flag_of(volume, Prop::bWaterVolume));
        let able = if in_water { self.store.flag_of(pawn, "bCanSwim") } else { self.store.flag_of(pawn, Prop::bCanWalk) || self.store.flag_of(pawn, Prop::bCanFly) };
        if !able {
            return false;
        }
        if sight == LineOfSight::Unknown {
            let eyes = [location[0], location[1], location[2] + self.store.float_of(pawn, Prop::EyeHeight)];
            if self.trace_world(eyes.into(), there, Vector::ZERO).is_some() {
                return false;
            }
        }
        if self.store.is_a(target, Kind::Pawn) {
            let radius = self.store.float_of(pawn, Prop::CollisionRadius);
            let reach = radius + self.store.float_of(pawn, Prop::MeleeRange).min(radius * 1.5) + self.store.float_of(target, Prop::CollisionRadius);
            if squared <= reach * reach {
                return true;
            }
        }
        self.reachable(pawn, location, there, Some(target))
    }

    pub(crate) fn point_reachable(&mut self, pawn: ObjectId, point: Vector, known_visible: bool) -> bool {
        let location = self.store.vector(pawn, Prop::Location);
        let (dx, dy) = (point[0] - location[0], point[1] - location[1]);
        if dx * dx + dy * dy > REACH_DISTANCE_SQUARED {
            return false;
        }
        if !known_visible {
            let eyes = [location[0], location[1], location[2] + self.store.float_of(pawn, Prop::EyeHeight)];
            if self.trace_world(eyes.into(), point, Vector::ZERO).is_some() {
                return false;
            }
        }
        self.reachable(pawn, location, point, None)
    }

    fn reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>) -> bool {
        let volume = self.store.object_of(pawn, Prop::PhysicsVolume);
        if volume.is_some_and(|volume| self.store.flag_of(volume, Prop::bWaterVolume)) {
            return self.swim_reachable(pawn, from, to, goal, HAND_OFFS);
        }
        if volume.is_some_and(|volume| self.store.is_a(volume, "LadderVolume")) {
            return self.ladder_reachable(pawn, from, to, goal);
        }
        match self.store.physics_of(pawn) {
            Some(EPhysics::Walking | EPhysics::Falling | EPhysics::Swimming | EPhysics::Ladder) => self.walk_reachable(pawn, from, to, goal),
            Some(EPhysics::Flying) => self.fly_reachable(pawn, from, to, goal, HAND_OFFS),
            _ => false,
        }
    }

    fn ladder_reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>) -> bool {
        if let Some(goal) = goal
            && let Some(ladder) = self.store.object_of(pawn, Prop::OnLadder)
        {
            let goal_ladder = if self.store.is_a(goal, "Ladder") {
                self.store.object_of(goal, "MyLadder")
            } else if self.store.physics_of(goal) == Some(EPhysics::Ladder) && self.store.is_a(goal, Kind::Pawn) {
                self.store.object_of(goal, Prop::OnLadder)
            } else {
                None
            };
            if goal_ladder == Some(ladder) {
                return true;
            }
        }
        self.walk_reachable(pawn, from, to, goal)
    }

    fn fly_reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>, hand_offs: u32) -> bool {
        let step_size = self.store.float_of(pawn, Prop::CollisionRadius).max(FLY_STEP);
        let mut location = from;
        for _ in 0..WALK_STEPS {
            let direction = to - location;
            if self.reached_destination(pawn, direction, goal) {
                return true;
            }
            let (step, threshold) = test_step(direction, step_size);
            let moved;
            (location, moved) = self.fly_move(pawn, location, step, threshold, Medium::Air);
            if !moved {
                return false;
            }
            if self.water_at(location) {
                return self.store.flag_of(pawn, Prop::bCanSwim) && !hurt_by_volume(&self.store, pawn, pawn) && hand_offs > 0 && self.swim_reachable(pawn, location, to, goal, hand_offs - 1);
            }
        }
        false
    }

    fn swim_reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>, hand_offs: u32) -> bool {
        let step_size = self.store.float_of(pawn, Prop::CollisionRadius).max(FLY_STEP);
        let mut location = from;
        for _ in 0..WALK_STEPS {
            let direction = to - location;
            if self.reached_destination(pawn, direction, goal) {
                return true;
            }
            let (step, threshold) = test_step(direction, step_size);
            let moved;
            (location, moved) = self.fly_move(pawn, location, step, threshold, Medium::Water);
            if !self.water_at(location) {
                return hand_offs > 0 && self.leave_water_reachable(pawn, location, to, goal, hand_offs - 1);
            }
            if !moved || hurt_by_volume(&self.store, pawn, pawn) {
                return false;
            }
        }
        false
    }

    fn leave_water_reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>, hand_offs: u32) -> bool {
        if self.store.flag_of(pawn, Prop::bCanFly) {
            return self.fly_reachable(pawn, from, to, goal, hand_offs);
        }
        if !self.store.flag_of(pawn, Prop::bCanWalk) || to.z >= from.z + SWIM_EXIT_HEIGHT {
            return false;
        }
        let height = self.store.float_of(pawn, Prop::CollisionHeight);
        let mut at = from;
        let rise = self.test_move(pawn, &mut at, [0.0, 0.0, (height + MAX_STEP_HEIGHT).max(to.z - from.z)].into());
        rise.time >= 1.0 && self.fly_reachable(pawn, at, to, goal, hand_offs)
    }

    fn fly_move(&mut self, pawn: ObjectId, from: Vector, step: Vector, threshold: f32, medium: Medium) -> (Vector, bool) {
        let mut at = from;
        let hit = self.test_move(pawn, &mut at, step);
        if medium == Medium::Water && !self.water_at(at) {
            let own = self.physics_volume_at(at, None).ok().flatten();
            let line = self.find_water_line(own, from, at);
            if line != at {
                let back = line - at;
                self.test_move(pawn, &mut at, back);
            }
            return (at, false);
        }
        if hit.time < 1.0 {
            let rest = step * (1.0 - hit.time);
            self.test_move(pawn, &mut at, [0.0, 0.0, MAX_STEP_HEIGHT].into());
            self.test_move(pawn, &mut at, rest);
        }
        let moved = at - from;
        (at, moved.dot(moved) >= threshold * threshold)
    }

    pub(crate) fn water_at(&mut self, point: Vector) -> bool {
        self.physics_volume_at(point, None).ok().flatten().is_some_and(|volume| self.store.flag_of(volume, Prop::bWaterVolume))
    }

    pub(crate) fn test_move(&mut self, pawn: ObjectId, at: &mut Vector, delta: Vector) -> MoveHit {
        let (radius, height) = self.store.collision_size(pawn);
        let hit = self.sweep(pawn, Sweep { start: *at, delta, radius, height }, SweepAgainst::WORLD, None);
        *at += delta * hit.time;
        hit
    }

    /// A failed step tries a jump when the pawn can jump; otherwise a fall shortens the steps to 35 once.
    fn walk_reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>) -> bool {
        let (radius, height) = self.store.collision_size(pawn);
        let can_jump = self.store.flag_of(pawn, Prop::bCanJump);
        let mut step_size = radius.max(if can_jump { 128.0 } else { 12.0 });
        let jump_target = goal.map_or(to, |goal| Vector::new(to.x, to.y, to.z - self.store.float_of(goal, Prop::CollisionHeight) + height));
        // Test moves leave the touch list alone, so a pawn already in a pain volume fails after its first step.
        let hurt = hurt_by_volume(&self.store, pawn, pawn);
        let mut location = from;
        for _ in 0..WALK_STEPS {
            let direction = to - location;
            if self.reached_destination(pawn, direction, goal) {
                return true;
            }
            if hurt {
                return false;
            }
            let (step, threshold) = test_step(Vector::new(direction.x, direction.y, 0.0), step_size);
            let ahead = location + step;
            let blocked = self.trace_bsp(location, ahead, [radius * 0.5, radius * 0.5, height * 0.5].into()).is_some()
                || self.trace_lines(location, ahead).is_some();
            let walked = if blocked {
                WalkMove::Stopped
            } else {
                let walked;
                (location, walked) = self.walk_move(pawn, location, step, threshold);
                walked
            };
            let mut answer = None;
            match walked {
                WalkMove::Moved => {}
                _ if self.store.flag_of(pawn, Prop::bCanFly) => answer = Some(self.fly_reachable(pawn, location, to, goal, HAND_OFFS)),
                WalkMove::Stopped if can_jump => {
                    let jumped;
                    (location, jumped) = self.find_jump_up(pawn, location, step);
                    answer = (!jumped).then_some(false);
                }
                WalkMove::Fell if can_jump => {
                    let jumped;
                    (location, jumped) = self.find_best_jump(pawn, location, jump_target);
                    answer = (!jumped).then_some(false);
                }
                WalkMove::Fell if step_size > MAX_STEP_HEIGHT => step_size = MAX_STEP_HEIGHT,
                WalkMove::Fell | WalkMove::Stopped => answer = Some(false),
            }
            let volume = self.physics_volume_at(location, None).ok().flatten();
            if volume.is_some_and(|volume| self.store.flag_of(volume, Prop::bWaterVolume)) {
                return if self.store.flag_of(pawn, Prop::bCanSwim) { self.swim_reachable(pawn, location, to, goal, HAND_OFFS) } else { answer.unwrap_or(false) };
            }
            if let Some(ladder) = volume.filter(|&volume| self.store.flag_of(pawn, Prop::bCanClimbLadders) && self.store.is_a(volume, "LadderVolume"))
                && let Some(goal) = goal.filter(|&goal| self.store.object_of(goal, Prop::PhysicsVolume) == Some(ladder))
            {
                let reach = height.max(self.store.float_of(goal, Prop::CollisionHeight));
                return self.store.vector(ladder, Prop::ClimbDir).dot(to - location).abs() < reach;
            }
            if let Some(answer) = answer {
                return answer;
            }
        }
        false
    }

    /// Where the test pawn is left, and the result; a fall or a bad floor after a step up puts it back to before its last move.
    pub(crate) fn walk_move(&mut self, pawn: ObjectId, from: Vector, step: Vector, threshold: f32) -> (Vector, WalkMove) {
        let (radius, height) = self.store.collision_size(pawn);
        let mut at = from;
        let step = Vector::new(step.x, step.y, 0.0);
        let volume = self.physics_volume_at(from, None).ok().flatten();
        let down = crate::physics::gravity_direction(volume.map_or(crate::physics::DEFAULT_GRAVITY, |volume| self.store.vector(volume, Prop::Gravity)));
        let hit = self.test_move(pawn, &mut at, step);
        if hit.time < 1.0 {
            let wall = at;
            let rest = step * (1.0 - hit.time);
            self.test_move(pawn, &mut at, down * -MAX_STEP_HEIGHT);
            self.test_move(pawn, &mut at, rest);
            let back = self.test_move(pawn, &mut at, down * MAX_STEP_HEIGHT);
            if back.time < 1.0 && back.normal[2] < MIN_FLOOR_Z {
                return (wall, WalkMove::Stopped);
            }
        }
        let ledge = at;
        let floor = self.test_move(pawn, &mut at, down * FLOOR_DROP);
        let embedded = self.find_floor(pawn, at, radius, height, 0.0).is_some_and(|f| f.distance <= 0.0 && f.normal[2] >= MIN_FLOOR_Z);
        if !embedded && (floor.time >= 1.0 || floor.normal[2] < MIN_FLOOR_Z) {
            return (ledge, WalkMove::Fell);
        }
        let moved = at - from;
        if moved.dot(moved) < threshold * threshold {
            return (at, WalkMove::Stopped);
        }
        (at, WalkMove::Moved)
    }
}

pub(crate) enum WalkMove {
    Moved,
    Stopped,
    Fell,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Medium {
    Air,
    Water,
}

fn test_step(direction: Vector, step_size: f32) -> (Vector, f32) {
    if direction.dot(direction) >= step_size * step_size {
        (direction.safe_normal() * step_size, WALK_MIN_MOVE)
    } else {
        (direction, WALK_MIN_LAST_MOVE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_cuts_long_moves_and_keeps_the_last_one() {
        let (step, threshold) = test_step(Vector::new(400.0, 0.0, 0.0), FLY_STEP);
        assert_eq!((step.x, step.y, step.z, threshold), (FLY_STEP, 0.0, 0.0, WALK_MIN_MOVE));
        let (step, threshold) = test_step(Vector::new(0.0, 30.0, 40.0), FLY_STEP);
        assert_eq!((step.x, step.y, step.z, threshold), (0.0, 30.0, 40.0, WALK_MIN_LAST_MOVE));
    }
}
