use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use p2_math::Vector;
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::Prop;
use crate::interp::{float, int};
use crate::natives::vec3;
use crate::physics::{MAX_STEP_HEIGHT, MIN_FLOOR_Z};
use crate::{Error, Kind, LoaderRef, ObjectId, Value, Vm};

const UNVISITED: i32 = 10_000_000;
const ANCHOR_DISTANCE_SQUARED: i32 = 1_440_000;
const REACH_DISTANCE_SQUARED: f32 = 1_000_000.0;
const WALK_STEPS: usize = 100;
const WALK_MIN_MOVE: f32 = 4.1;
const WALK_MIN_LAST_MOVE: f32 = 8.0;
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

    fn reach<L: LoaderRef>(&mut self, vm: &Vm<L>, node: usize, actor: ObjectId) {
        if self.stamps[node] != self.query {
            self.stamps[node] = self.query;
            self.weight[node] = UNVISITED;
            self.previous[node] = None;
            self.blocked[node] = vm.flag_of(actor, Prop::bBlocked);
            self.cost[node] = vm.get(actor, Prop::ExtraCost).as_ref().map_or(0, int);
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
    index: HashMap<ObjectId, usize>,
    search: Search,
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

impl<L: LoaderRef> Vm<L> {
    fn graph(&mut self) -> &Graph {
        if self.nav_graph.is_none() {
            let graph = self.read_graph();
            self.nav_graph = Some(graph);
        }
        self.nav_graph.get_or_insert_with(Graph::default)
    }

    fn read_graph(&self) -> Graph {
        let mut graph = Graph::default();
        let mut current = self.level.and_then(|level| self.object_of(level, Prop::NavigationPointList));
        let mut actors = Vec::new();
        while let Some(nav) = current {
            if graph.index.contains_key(&nav) {
                break;
            }
            graph.index.insert(nav, actors.len());
            actors.push(nav);
            current = self.object_of(nav, Prop::nextNavigationPoint);
        }
        for &actor in &actors {
            let specs = match self.get(actor, Prop::PathList) {
                Some(Value::Array(list)) => list
                    .iter()
                    .filter_map(|v| if let &Value::Object(Some(spec)) = v { Some(spec) } else { None })
                    .filter_map(|spec| {
                        let end = *graph.index.get(&self.object_of(spec, Prop::End)?)?;
                        let number = |name: &str| self.get(spec, name).as_ref().map_or(0, int);
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
                location: self.vector(actor, Prop::Location),
                radius: self.float_of(actor, Prop::CollisionRadius),
                height: self.float_of(actor, Prop::CollisionHeight),
                specs,
            });
        }
        graph
    }

    pub(crate) fn nav_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(None) };
        let object = |n: usize| crate::natives::object_arg(args, n);
        Ok(Some(match native {
            Native::FindPathToward => {
                let Some(goal) = object(0) else {
                    self.diagnostics.log.push(format!("Warning: No goal for FindPathToward by {}", self.object(this).path));
                    return Ok(Some(Value::Object(None)));
                };
                Value::Object(self.find_path(this, Some(goal), Vector::ZERO)?)
            }
            Native::FindPathTo => Value::Object(self.find_path(this, None, vec3(args.first()))?),
            Native::FindRandomDest => {
                let Some(pawn) = self.object_of(this, Prop::Pawn) else { return Ok(Some(Value::Object(None))) };
                let weight = self.find_path_toward(this, pawn, None, Vector::ZERO, Evaluator::Random)?;
                let goal = self.object_of(this, Prop::RouteGoal).filter(|&g| weight > 0.0 && self.is_a(g, Kind::NavigationPoint));
                Value::Object(goal)
            }
            Native::actorReachable => {
                let reachable = match (self.object_of(this, Prop::Pawn), object(0)) {
                    (Some(pawn), Some(target)) => self.actor_reachable(pawn, target, false, false),
                    _ => false,
                };
                Value::Bool(reachable)
            }
            Native::pointReachable => {
                let reachable = match self.object_of(this, Prop::Pawn) {
                    Some(pawn) => self.point_reachable(pawn, vec3(args.first()), false),
                    None => false,
                };
                Value::Bool(reachable)
            }
            Native::ClearPaths => return Ok(None),
            Native::BuildPathNodeLists | Native::BuildHomeNodeLists => {
                self.build_node_lists(this, native == Native::BuildHomeNodeLists);
                return Ok(None);
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }))
    }

    pub(crate) fn find_path(&mut self, controller: ObjectId, goal: Option<ObjectId>, location: Vector) -> Result<Option<ObjectId>, Error> {
        let Some(pawn) = self.object_of(controller, Prop::Pawn) else {
            self.diagnostics.log.push(format!("Warning: No pawn for FindPath by {}", self.object(controller).path));
            return Ok(None);
        };
        self.set(controller, Prop::bPreparingMove, Value::Bool(false));
        let weight = self.find_path_toward(controller, pawn, goal, location, Evaluator::EndPoint(None))?;
        if weight <= 0.0 {
            return Ok(None);
        }
        Ok(match self.get(controller, Prop::RouteCache) {
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
        self.set(pawn, Prop::NextPathRadius, Value::Float(0.0));
        let specified_end = matches!(evaluator, Evaluator::EndPoint(_));
        let mut goal_location = goal_location;
        let mut end_anchor = None;
        let mut end_distance = 0.0;
        if let Some(goal) = goal {
            goal_location = self.vector(goal, Prop::Location);
            if self.physics_of(pawn) == Some(EPhysics::Walking) {
                let (radius, height) = (self.float_of(pawn, Prop::CollisionRadius), self.float_of(pawn, Prop::CollisionHeight));
                let hit = self.sweep(pawn, goal_location, [0.0, 0.0, -1000.0].into(), radius, height, false);
                goal_location[2] -= 1000.0 * hit.time;
            }
            if let Some(&node) = self.graph().index.get(&goal) {
                end_anchor = Some(node);
            } else if self.is_a(goal, Kind::Pawn) && self.valid_anchor(goal) {
                let anchor = self.object_of(goal, Prop::Anchor).and_then(|a| self.graph().index.get(&a).copied());
                if let Some(anchor) = anchor {
                    end_anchor = Some(anchor);
                    end_distance = (self.graph().nodes[anchor].location - goal_location).size();
                }
            }
        }
        if !self.valid_anchor(pawn) {
            self.set(pawn, Prop::Anchor, Value::Object(None));
        }
        let mut anchor = self.object_of(pawn, Prop::Anchor).and_then(|a| self.graph().index.get(&a).copied());
        let mut start_distance = 0.0;
        if anchor.is_none() || (end_anchor.is_none() && specified_end) {
            let location = self.vector(pawn, Prop::Location);
            if anchor.is_none() {
                let Some(found) = self.find_start_anchor(pawn, location) else { return Ok(0.0) };
                let node = &self.graph().nodes[found];
                let (actor, node_location, node_radius) = (node.actor, node.location, node.radius);
                self.set(pawn, Prop::Anchor, Value::Object(Some(actor)));
                start_distance = (node_location - location).size();
                if start_distance <= node_radius + self.float_of(pawn, Prop::CollisionRadius) {
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
        let Searched { best, best_weight, search } = self.breadth_path_to(pawn, start, start_distance as i32, evaluator);
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
        let location = self.vector(pawn, Prop::Location);
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
        self.set(controller, Prop::RouteCache, Value::Array(cache));
        (goal_location - location).size()
    }

    fn nodes_near(&self, location: Vector) -> Vec<usize> {
        let graph = self.nav_graph.as_ref();
        let mut near: Vec<(i32, usize)> = graph
            .map(|g| g.nodes.iter().enumerate())
            .into_iter()
            .flatten()
            .filter_map(|(i, node)| {
                let offset = location - node.location;
                let squared = offset.dot(offset) as i32;
                (squared < ANCHOR_DISTANCE_SQUARED && !self.flag_of(node.actor, Prop::bBlocked)).then_some((squared, i))
            })
            .collect();
        near.sort();
        near.into_iter().map(|(_, i)| i).collect()
    }

    fn find_start_anchor(&mut self, pawn: ObjectId, location: Vector) -> Option<usize> {
        let height = self.float_of(pawn, Prop::CollisionHeight);
        for candidate in self.nodes_near(location) {
            let node = &self.graph().nodes[candidate];
            let (actor, node_location, node_height) = (node.actor, node.location, node.height);
            let visible = self.trace_world(node_location, location, Vector::ZERO).is_none()
                || self.trace_world(Vector::new(node_location.x, node_location.y, node_location.z + node_height), Vector::new(location.x, location.y, location.z + height), Vector::ZERO).is_none();
            if visible && self.actor_reachable(pawn, actor, true, true) {
                return Some(candidate);
            }
        }
        None
    }

    fn find_end_anchor(&mut self, pawn: ObjectId, goal: Option<ObjectId>, goal_location: Vector) -> Option<usize> {
        let height = self.float_of(pawn, Prop::CollisionHeight);
        for candidate in self.nodes_near(goal_location) {
            let node = &self.graph().nodes[candidate];
            let (node_location, node_height) = (node.location, node.height);
            let goal_top = [goal_location[0], goal_location[1], goal_location[2] + goal.map_or(0.0, |g| self.float_of(g, Prop::CollisionHeight))];
            let visible = self.trace_world(goal_location, node_location, Vector::ZERO).is_none()
                || self.trace_world(goal_top.into(), Vector::new(node_location.x, node_location.y, node_location.z + node_height), Vector::ZERO).is_none();
            if !visible {
                continue;
            }
            let standing = [node_location.x, node_location.y, node_location.z + height - node_height];
            let reachable = match goal {
                Some(goal) => self.walk_reachable(pawn, standing.into(), self.vector(goal, Prop::Location), Some(goal)),
                None => self.walk_reachable(pawn, standing.into(), goal_location, None),
            };
            if reachable {
                return Some(candidate);
            }
        }
        None
    }

    fn valid_anchor(&self, pawn: ObjectId) -> bool {
        let Some(anchor) = self.object_of(pawn, Prop::Anchor) else { return false };
        !self.flag_of(anchor, Prop::bBlocked)
            && self.reached_destination(pawn, self.vector(anchor, Prop::Location) - self.vector(pawn, Prop::Location), Some(anchor))
    }

    fn move_flags(&self, pawn: ObjectId) -> i32 {
        let controller = self.object_of(pawn, Prop::Controller);
        let pawn_flag = |name: Prop| i32::from(self.flag_of(pawn, name));
        let controller_flag = |name: Prop| controller.map_or(0, |c| i32::from(self.flag_of(c, name)));
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

    fn breadth_path_to(&mut self, pawn: ObjectId, start: usize, start_weight: i32, evaluator: Evaluator) -> Searched {
        let can_crouch = self.flag_of(pawn, Prop::bCanCrouch);
        let (radius, height) = match can_crouch {
            true => (self.float_of(pawn, Prop::CrouchRadius), self.float_of(pawn, Prop::CrouchHeight)),
            false => (self.float_of(pawn, Prop::CollisionRadius), self.float_of(pawn, Prop::CollisionHeight)),
        };
        let (radius, height) = (radius as i32, height as i32);
        let max_fall = self.float_of(pawn, Prop::MaxFallSpeed) as i32;
        let crouch_cost = 1.1 / self.float_of(pawn, Prop::WalkingPct);
        let default_height = self.object(pawn).class.and_then(|c| self.default_value(c, Prop::CollisionHeight)).as_ref().map_or(0.0, float);
        let flags = self.move_flags(pawn);
        let mut graph = self.nav_graph.take().unwrap_or_default();
        let mut search = std::mem::take(&mut graph.search);
        search.start(graph.nodes.len());
        search.reach(&*self, start, graph.nodes[start].actor);
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
                Evaluator::Random => self.random(),
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
            if search.blocked[current] {
                continue;
            }
            for spec in &graph.nodes[current].specs {
                let supported = spec.radius >= radius && spec.height >= height && spec.flags & flags == spec.flags && spec.max_landing <= max_fall;
                if !supported || spec.end == current {
                    continue;
                }
                search.reach(&*self, spec.end, graph.nodes[spec.end].actor);
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
        self.nav_graph = Some(graph);
        Searched { best, best_weight, search }
    }

    fn set_route_cache(&mut self, controller: ObjectId, pawn: ObjectId, found: &FoundRoute) {
        let FoundRoute { best, weight, previous, start_distance, end_distance } = *found;
        let mut route = vec![best];
        while let Some(&Some(before)) = previous.get(route[route.len() - 1]) {
            if route.len() > previous.len() {
                break;
            }
            route.push(before);
        }
        route.reverse();
        let nodes: Vec<(ObjectId, Vector)> = route.iter().map(|&n| (self.graph().nodes[n].actor, self.graph().nodes[n].location)).collect();
        self.set(controller, Prop::RouteGoal, Value::Object(Some(nodes[nodes.len() - 1].0)));
        self.set(controller, Prop::RouteDist, Value::Float(weight[best] as f32 + end_distance));
        let mut first = 0;
        if let Some(&(next, next_location)) = nodes.get(1) {
            first = 1;
            if start_distance > 0.0 {
                let location = self.vector(pawn, Prop::Location);
                let to_pawn = (location - next_location).size();
                let along = (nodes[0].1 - next_location).size();
                let skip = to_pawn < SKIP_ANCHOR_DISTANCE
                    && to_pawn < (along + start_distance) * SKIP_ANCHOR_SHARE
                    && self.trace_world(next_location, location, Vector::ZERO).is_none()
                    && self.actor_reachable(pawn, next, true, true);
                first = usize::from(skip);
            }
        }
        let cache: Vec<Value> = (0..ROUTE_CACHE).map(|i| Value::Object(nodes.get(first + i).map(|n| n.0))).collect();
        self.set(controller, Prop::RouteCache, Value::Array(cache));
        if let (Some(&a), Some(&b)) = (route.get(first), route.get(first + 1))
            && let Some(spec) = self.graph().nodes[a].specs.iter().find(|s| s.end == b)
        {
            let radius = spec.radius as f32;
            self.set(pawn, Prop::NextPathRadius, Value::Float(radius));
        }
    }

    pub(crate) fn actor_reachable(&mut self, pawn: ObjectId, target: ObjectId, known_visible: bool, no_anchor_check: bool) -> bool {
        if self.is_deleted(target) {
            return false;
        }
        let location = self.vector(pawn, Prop::Location);
        let there = self.vector(target, Prop::Location);
        if !no_anchor_check && self.object_of(pawn, Prop::Anchor) == Some(target) && self.valid_anchor(pawn) {
            return true;
        }
        let offset = there - location;
        let squared = offset.dot(offset);
        if squared > REACH_DISTANCE_SQUARED || !(self.flag_of(pawn, Prop::bCanWalk) || self.flag_of(pawn, Prop::bCanFly)) {
            return false;
        }
        if !known_visible {
            let eyes = [location[0], location[1], location[2] + self.float_of(pawn, Prop::EyeHeight)];
            if self.trace_world(eyes.into(), there, Vector::ZERO).is_some() {
                return false;
            }
        }
        if self.is_a(target, Kind::Pawn) {
            let radius = self.float_of(pawn, Prop::CollisionRadius);
            let reach = radius + self.float_of(pawn, Prop::MeleeRange).min(radius * 1.5) + self.float_of(target, Prop::CollisionRadius);
            if squared <= reach * reach {
                return true;
            }
        }
        self.reachable(pawn, location, there, Some(target))
    }

    pub(crate) fn point_reachable(&mut self, pawn: ObjectId, point: Vector, known_visible: bool) -> bool {
        let location = self.vector(pawn, Prop::Location);
        let (dx, dy) = (point[0] - location[0], point[1] - location[1]);
        if dx * dx + dy * dy > REACH_DISTANCE_SQUARED {
            return false;
        }
        if !known_visible {
            let eyes = [location[0], location[1], location[2] + self.float_of(pawn, Prop::EyeHeight)];
            if self.trace_world(eyes.into(), point, Vector::ZERO).is_some() {
                return false;
            }
        }
        self.reachable(pawn, location, point, None)
    }

    fn reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>) -> bool {
        if self.physics_of(pawn) == Some(EPhysics::Flying) {
            return self.trace_world(from, to, Vector::ZERO).is_none();
        }
        self.walk_reachable(pawn, from, to, goal)
    }

    /// A fall shortens the steps to 35 once; jumps are not tried (unverified).
    fn walk_reachable(&mut self, pawn: ObjectId, from: Vector, to: Vector, goal: Option<ObjectId>) -> bool {
        let (radius, height) = (self.float_of(pawn, Prop::CollisionRadius), self.float_of(pawn, Prop::CollisionHeight));
        let mut step_size = radius.max(if self.flag_of(pawn, Prop::bCanJump) { 128.0 } else { 12.0 });
        let mut location = from;
        for _ in 0..WALK_STEPS {
            let direction = to - location;
            if self.reached_destination(pawn, direction, goal) {
                return true;
            }
            let flat = Vector::new(direction.x, direction.y, 0.0);
            let (step, threshold) = if flat.dot(flat) >= step_size * step_size {
                ((flat.safe_normal() * step_size), WALK_MIN_MOVE)
            } else {
                (flat, WALK_MIN_LAST_MOVE)
            };
            let ahead = location + step;
            if self.trace_bsp(location, ahead, [radius * 0.5, radius * 0.5, height * 0.5].into()).is_some()
                || self.trace_lines(location, ahead).is_some()
            {
                return false;
            }
            match self.walk_move(pawn, location, step, threshold) {
                WalkMove::Moved(next) => location = next,
                WalkMove::Fell if step_size > MAX_STEP_HEIGHT => step_size = MAX_STEP_HEIGHT,
                WalkMove::Fell | WalkMove::Stopped => return false,
            }
        }
        false
    }

    fn walk_move(&mut self, pawn: ObjectId, from: Vector, step: Vector, threshold: f32) -> WalkMove {
        let (radius, height) = (self.float_of(pawn, Prop::CollisionRadius), self.float_of(pawn, Prop::CollisionHeight));
        let go = |vm: &mut Self, at: &mut Vector, delta: Vector| {
            let hit = vm.sweep(pawn, *at, delta, radius, height, false);
            *at += delta * hit.time;
            hit
        };
        let mut at = from;
        let step = Vector::new(step.x, step.y, 0.0);
        let hit = go(self, &mut at, step);
        if hit.time < 1.0 {
            let rest = step * (1.0 - hit.time);
            go(self, &mut at, [0.0, 0.0, MAX_STEP_HEIGHT].into());
            go(self, &mut at, rest);
            let down = go(self, &mut at, [0.0, 0.0, -MAX_STEP_HEIGHT].into());
            if down.time < 1.0 && down.normal[2] < MIN_FLOOR_Z {
                return WalkMove::Stopped;
            }
        }
        let floor = go(self, &mut at, [0.0, 0.0, -MAX_STEP_HEIGHT].into());
        let embedded = self.find_floor(pawn, at, radius, height, 0.0).is_some_and(|f| f.distance <= 0.0 && f.normal[2] >= MIN_FLOOR_Z);
        if !embedded && (floor.time >= 1.0 || floor.normal[2] < MIN_FLOOR_Z) {
            return WalkMove::Fell;
        }
        let moved = at - from;
        if moved.dot(moved) < threshold * threshold {
            return WalkMove::Stopped;
        }
        WalkMove::Moved(at)
    }

    fn build_node_lists(&mut self, game: ObjectId, homes: bool) {
        let (path_node, home_node) = (self.script.lookups.class_names.path_node, self.script.lookups.class_names.home_node);
        let nodes: Vec<ObjectId> = self
            .actors()
            .filter(|&o| self.is_a(o, path_node) && !self.is_deleted(o) && self.is_a(o, home_node) == homes)
            .collect();
        let mut lists: Vec<Vec<ObjectId>> = Vec::new();
        for node in nodes {
            let tag = self.get(node, Prop::Tag);
            let found = lists.iter().position(|list| !homes || self.get(list[0], Prop::Tag) == tag);
            match found {
                Some(list) => lists[list].push(node),
                None => lists.push(vec![node]),
            }
        }
        let mut entries = Vec::new();
        for list in &lists {
            for (i, &node) in list.iter().enumerate() {
                self.set(node, Prop::NextNode, Value::Object(Some(list[(i + 1) % list.len()])));
            }
            entries.push(Value::Struct(vec![Value::Object(Some(list[0])), Value::Int(list.len() as i32)]));
        }
        if homes {
            self.set(game, Prop::HomeNodeLists, Value::Array(entries));
        } else if let Some(entry) = entries.pop() {
            self.set(game, Prop::PathNodeList, entry);
        }
    }
}

enum WalkMove {
    Moved(Vector),
    Stopped,
    Fell,
}
