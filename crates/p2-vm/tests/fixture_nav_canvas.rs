mod common;

use std::sync::Arc;

use common::load_fixture;
use p2_math::{Rotator, Vector};
use p2_package::Loader;
use p2_script::flags::{CPF_PARM, FUNC_NATIVE};
use p2_test_package::token::{CONTEXT, INSTANCE_VARIABLE, LOCAL_VARIABLE, NOTHING, RETURN};
use p2_test_package::{DefaultValue, PackageBuilder, PropertyType, Script};
use p2_vm::{CanvasItem, ObjectId, Value, Vm};

const PHYS_WALKING: u8 = 1;
const PHYS_FLYING: u8 = 4;
const PAWN_RADIUS: f32 = 20.0;
const PAWN_HEIGHT: f32 = 40.0;
const NODE_RADIUS: f32 = 40.0;
const NODE_HEIGHT: f32 = 40.0;
const SPEC_HEIGHT: i32 = 100;
const NO_DOORS_FLAG: i32 = 16;

const CHAIN_A: Vector = Vector::new(0.0, 0.0, 0.0);
const CHAIN_B: Vector = Vector::new(500.0, 0.0, 0.0);
const CHAIN_C: Vector = Vector::new(1000.0, 0.0, 0.0);
const CHAIN_EDGES: [(usize, usize, i32, i32); 2] = [(0, 1, 500, 100), (1, 2, 500, 60)];

const FORK_START: Vector = Vector::new(0.0, 0.0, 0.0);
const FORK_SHORT: Vector = Vector::new(100.0, 100.0, 0.0);
const FORK_LONG: Vector = Vector::new(50.0, -50.0, 0.0);
const FORK_GOAL: Vector = Vector::new(200.0, 0.0, 0.0);
const FORK_EDGES: [(usize, usize, i32, i32); 4] = [(0, 1, 100, 100), (1, 3, 100, 70), (0, 2, 50, 100), (2, 3, 400, 90)];

const END_FUNCTION_PARMS: u8 = 0x16;
const FINAL_FUNCTION: u8 = 0x1c;
const FLOAT_CONST: u8 = 0x1e;

fn declare_vector(package: &mut PackageBuilder) -> i32 {
    let vector = package.struct_type("Vector");
    for member in ["X", "Y", "Z"] {
        package.property(vector, member, PropertyType::Float, 0);
    }
    vector
}

fn graph_package() -> PackageBuilder {
    let mut package = PackageBuilder::new("Fixture");
    let vector = declare_vector(&mut package);
    let actor = package.class("Actor", None);
    package.property(actor, "Location", PropertyType::Struct(vector), 0);
    package.property(actor, "Physics", PropertyType::Byte, 0);
    package.property(actor, "Tag", PropertyType::Name, 0);
    package.property(actor, "CollisionRadius", PropertyType::Float, 0);
    package.property(actor, "CollisionHeight", PropertyType::Float, 0);

    let navigation_point = package.class("NavigationPoint", Some(actor));
    package.property(navigation_point, "nextNavigationPoint", PropertyType::Object, 0);
    package.array_property(navigation_point, "PathList", PropertyType::Object, 0);
    package.property(navigation_point, "bBlocked", PropertyType::Bool, 0);
    package.property(navigation_point, "ExtraCost", PropertyType::Int, 0);

    let path_node = package.class("PathNode", Some(navigation_point));
    package.property(path_node, "NextNode", PropertyType::Object, 0);
    package.default_value(path_node, "CollisionRadius", DefaultValue::Float(NODE_RADIUS));
    package.default_value(path_node, "CollisionHeight", DefaultValue::Float(NODE_HEIGHT));
    package.class("HomeNode", Some(path_node));

    let pawn = package.class("Pawn", Some(actor));
    package.property(pawn, "Anchor", PropertyType::Object, 0);
    package.property(pawn, "bCanWalk", PropertyType::Bool, 0);
    package.property(pawn, "bCanFly", PropertyType::Bool, 0);
    package.property(pawn, "NextPathRadius", PropertyType::Float, 0);
    package.property(pawn, "MeleeRange", PropertyType::Float, 0);
    package.default_value(pawn, "CollisionRadius", DefaultValue::Float(PAWN_RADIUS));
    package.default_value(pawn, "CollisionHeight", DefaultValue::Float(PAWN_HEIGHT));
    package.default_value(pawn, "Physics", DefaultValue::Byte(PHYS_FLYING));
    package.default_value(pawn, "bCanFly", DefaultValue::Bool(true));

    let controller = package.class("Controller", None);
    package.property(controller, "Pawn", PropertyType::Object, 0);
    package.array_property(controller, "RouteCache", PropertyType::Object, 0);
    package.property(controller, "RouteGoal", PropertyType::Object, 0);
    package.property(controller, "RouteDist", PropertyType::Float, 0);
    package.property(controller, "bPreparingMove", PropertyType::Bool, 0);

    let spec = package.class("ReachSpec", None);
    package.property(spec, "End", PropertyType::Object, 0);
    for name in ["Distance", "CollisionRadius", "CollisionHeight", "reachFlags", "MaxLandingVelocity"] {
        package.property(spec, name, PropertyType::Int, 0);
    }

    let level = package.class("LevelInfo", None);
    package.property(level, "NavigationPointList", PropertyType::Object, 0);

    let node_list = package.struct_type("NodeList");
    package.property(node_list, "First", PropertyType::Object, 0);
    package.property(node_list, "Count", PropertyType::Int, 0);
    let game = package.class("GameInfo", None);
    package.property(game, "PathNodeList", PropertyType::Struct(node_list), 0);
    package.array_property(game, "HomeNodeLists", PropertyType::Struct(node_list), 0);
    package
}

struct Graph {
    vm: Vm<Arc<Loader>>,
    controller: ObjectId,
    pawn: ObjectId,
    nodes: Vec<ObjectId>,
    specs: Vec<ObjectId>,
}

fn spawn_at(vm: &mut Vm<Arc<Loader>>, class_name: &str, at: Vector) -> ObjectId {
    let class = vm.find_class("Fixture", class_name).unwrap();
    vm.spawn(class, None, [Some(Value::Vector(at)), None]).unwrap().unwrap()
}

fn unlinked_graph(dir: &str, pawn_at: Vector, locations: &[Vector], edges: &[(usize, usize, i32, i32)]) -> (Graph, ObjectId) {
    let mut vm = load_fixture(&graph_package(), dir);
    let nodes: Vec<ObjectId> = locations.iter().map(|&at| spawn_at(&mut vm, "PathNode", at)).collect();
    for pair in nodes.windows(2) {
        assert!(vm.store.set(pair[0], "nextNavigationPoint", Value::Object(Some(pair[1]))));
    }
    let spec_class = vm.find_class("Fixture", "ReachSpec").unwrap();
    let mut specs = Vec::new();
    let mut paths = vec![Vec::new(); nodes.len()];
    for &(from, to, distance, radius) in edges {
        let spec = vm.new_object(spec_class, None);
        assert!(vm.store.set(spec, "End", Value::Object(Some(nodes[to]))));
        assert!(vm.store.set(spec, "Distance", Value::Int(distance)));
        assert!(vm.store.set(spec, "CollisionRadius", Value::Int(radius)));
        assert!(vm.store.set(spec, "CollisionHeight", Value::Int(SPEC_HEIGHT)));
        specs.push(spec);
        paths[from].push(Value::Object(Some(spec)));
    }
    for (&node, path) in nodes.iter().zip(paths) {
        assert!(vm.store.set(node, "PathList", Value::Array(path)));
    }
    let pawn = spawn_at(&mut vm, "Pawn", pawn_at);
    let controller_class = vm.find_class("Fixture", "Controller").unwrap();
    let controller = vm.new_object(controller_class, None);
    assert!(vm.store.set(controller, "Pawn", Value::Object(Some(pawn))));
    let level_class = vm.find_class("Fixture", "LevelInfo").unwrap();
    let level = vm.new_object(level_class, None);
    if let Some(&first) = nodes.first() {
        assert!(vm.store.set(level, "NavigationPointList", Value::Object(Some(first))));
    }
    (Graph { vm, controller, pawn, nodes, specs }, level)
}

fn graph(dir: &str, pawn_at: Vector, locations: &[Vector], edges: &[(usize, usize, i32, i32)]) -> Graph {
    let (mut graph, level) = unlinked_graph(dir, pawn_at, locations, edges);
    graph.vm.set_level(level);
    graph
}

fn chain(dir: &str, pawn_at: Vector) -> Graph {
    graph(dir, pawn_at, &[CHAIN_A, CHAIN_B, CHAIN_C], &CHAIN_EDGES)
}

fn fork(dir: &str) -> Graph {
    graph(dir, FORK_START, &[FORK_START, FORK_SHORT, FORK_LONG, FORK_GOAL], &FORK_EDGES)
}

impl Graph {
    fn native(&mut self, name: &str, arg: Value) -> Option<Value> {
        self.vm.call_native(name, Some(self.controller), &mut [arg]).unwrap()
    }

    fn path_toward(&mut self, goal: ObjectId) -> Option<Value> {
        self.native("FindPathToward", Value::Object(Some(goal)))
    }

    fn route(&self) -> Vec<ObjectId> {
        let Some(Value::Array(cache)) = self.vm.store.get(self.controller, "RouteCache") else { panic!("no route cache") };
        cache.iter().filter_map(|entry| if let &Value::Object(node) = entry { node } else { None }).collect()
    }

    fn assert_route(&self, first_hops: &[ObjectId], distance: f32) {
        assert_eq!(self.route(), first_hops);
        assert_eq!(self.vm.store.get(self.controller, "RouteDist"), Some(Value::Float(distance)));
    }

    fn make_walker(&mut self) {
        assert!(self.vm.store.set(self.pawn, "Physics", Value::Byte(PHYS_WALKING)));
        assert!(self.vm.store.set(self.pawn, "bCanWalk", Value::Bool(true)));
        assert!(self.vm.store.set(self.pawn, "bCanFly", Value::Bool(false)));
    }
}

#[test]
fn path_along_a_chain_returns_the_next_node_and_fills_the_route() {
    let mut graph = chain("nav-chain", CHAIN_A);
    let [a, b, c] = graph.nodes[..] else { panic!("three nodes") };
    assert!(graph.vm.store.set(graph.controller, "bPreparingMove", Value::Bool(true)));

    assert_eq!(graph.path_toward(c), Some(Value::Object(Some(b))));

    graph.assert_route(&[b, c], 1000.0);
    let Some(Value::Array(cache)) = graph.vm.store.get(graph.controller, "RouteCache") else { panic!("no route cache") };
    assert_eq!(cache.len(), 16);
    assert_eq!(graph.vm.store.get(graph.controller, "RouteGoal"), Some(Value::Object(Some(c))));
    assert_eq!(graph.vm.store.get(graph.controller, "bPreparingMove"), Some(Value::Bool(false)));
    assert_eq!(graph.vm.store.get(graph.pawn, "Anchor"), Some(Value::Object(Some(a))));
    assert_eq!(graph.vm.store.get(graph.pawn, "NextPathRadius"), Some(Value::Float(60.0)));
}

#[test]
fn path_through_a_fork_takes_the_shorter_total_not_the_shorter_first_hop() {
    let mut graph = fork("nav-fork");
    let [_, short, _, goal] = graph.nodes[..] else { panic!("four nodes") };

    assert_eq!(graph.path_toward(goal), Some(Value::Object(Some(short))));

    graph.assert_route(&[short, goal], 200.0);
    assert_eq!(graph.vm.store.get(graph.pawn, "NextPathRadius"), Some(Value::Float(70.0)));
}

#[test]
fn extra_cost_on_a_node_moves_the_path_to_the_other_branch() {
    let mut graph = fork("nav-extra-cost");
    let [_, short, long, goal] = graph.nodes[..] else { panic!("four nodes") };
    assert!(graph.vm.store.set(short, "ExtraCost", Value::Int(300)));

    assert_eq!(graph.path_toward(goal), Some(Value::Object(Some(long))));

    graph.assert_route(&[long, goal], 450.0);
    assert_eq!(graph.vm.store.get(graph.pawn, "NextPathRadius"), Some(Value::Float(90.0)));
}

#[test]
fn a_blocked_node_is_not_passed_through() {
    let mut graph = fork("nav-blocked");
    let [_, short, long, goal] = graph.nodes[..] else { panic!("four nodes") };
    assert!(graph.vm.store.set(short, "bBlocked", Value::Bool(true)));

    assert_eq!(graph.path_toward(goal), Some(Value::Object(Some(long))));

    graph.assert_route(&[long, goal], 450.0);
}

#[test]
fn a_spec_the_pawn_cannot_use_is_not_taken() {
    let cases = [
        ("nav-spec-narrow", "CollisionRadius", 10),
        ("nav-spec-low", "CollisionHeight", 10),
        ("nav-spec-flags", "reachFlags", NO_DOORS_FLAG),
        ("nav-spec-landing", "MaxLandingVelocity", 100),
    ];
    for (dir, property, value) in cases {
        let mut graph = fork(dir);
        let [_, _, long, goal] = graph.nodes[..] else { panic!("four nodes") };
        let start_to_short = graph.specs[0];
        assert!(graph.vm.store.set(start_to_short, property, Value::Int(value)));

        assert_eq!(graph.path_toward(goal), Some(Value::Object(Some(long))), "{property}");

        graph.assert_route(&[long, goal], 450.0);
    }
}

#[test]
fn a_goal_with_no_way_there_gives_no_path() {
    let mut graph = graph("nav-unreachable", CHAIN_A, &[CHAIN_A, CHAIN_C], &[]);
    let goal = graph.nodes[1];

    assert_eq!(graph.path_toward(goal), Some(Value::Object(None)));

    assert!(graph.route().is_empty());
    assert_eq!(graph.vm.store.get(graph.controller, "RouteGoal"), Some(Value::Object(None)));
}

#[test]
fn no_goal_gives_no_path() {
    let mut graph = chain("nav-no-goal", CHAIN_A);
    assert_eq!(graph.native("FindPathToward", Value::Object(None)), Some(Value::Object(None)));
}

#[test]
fn a_controller_without_a_pawn_gives_no_path() {
    let mut graph = chain("nav-no-pawn", CHAIN_A);
    let goal = graph.nodes[2];
    assert!(graph.vm.store.set(graph.controller, "Pawn", Value::Object(None)));
    assert_eq!(graph.path_toward(goal), Some(Value::Object(None)));
    assert_eq!(graph.native("FindRandomDest", Value::Map), Some(Value::Object(None)));
    assert_eq!(graph.native("actorReachable", Value::Object(Some(goal))), Some(Value::Bool(false)));
    assert_eq!(graph.native("pointReachable", Value::Vector(CHAIN_A)), Some(Value::Bool(false)));
}

#[test]
fn without_a_level_there_is_no_graph_and_no_path() {
    let (mut graph, _level) = unlinked_graph("nav-no-level", CHAIN_A, &[CHAIN_A, CHAIN_B, CHAIN_C], &CHAIN_EDGES);
    let goal = graph.nodes[2];
    assert_eq!(graph.path_toward(goal), Some(Value::Object(None)));
}

#[test]
fn a_goal_the_pawn_is_already_touching_is_returned_itself() {
    let at = Vector::new(10.0, 0.0, 0.0);
    let mut graph = graph("nav-same-anchor", at, &[CHAIN_A], &[]);
    let goal = graph.nodes[0];

    assert_eq!(graph.path_toward(goal), Some(Value::Object(Some(goal))));

    assert_eq!(graph.route(), [goal]);
    assert_eq!(graph.vm.store.get(graph.pawn, "Anchor"), Some(Value::Object(Some(goal))));
}

#[test]
fn a_pawn_past_its_anchor_skips_it() {
    let mut graph = chain("nav-skip-anchor", Vector::new(100.0, 0.0, 0.0));
    let [_, b, c] = graph.nodes[..] else { panic!("three nodes") };

    assert_eq!(graph.path_toward(c), Some(Value::Object(Some(b))));

    graph.assert_route(&[b, c], 1100.0);
    assert_eq!(graph.vm.store.get(graph.pawn, "NextPathRadius"), Some(Value::Float(60.0)));
}

#[test]
fn a_pawn_behind_its_anchor_goes_to_it_first() {
    let mut graph = chain("nav-keep-anchor", Vector::new(-100.0, 0.0, 0.0));
    let [a, b, c] = graph.nodes[..] else { panic!("three nodes") };

    assert_eq!(graph.path_toward(c), Some(Value::Object(Some(a))));

    graph.assert_route(&[a, b, c], 1100.0);
    assert_eq!(graph.vm.store.get(graph.pawn, "NextPathRadius"), Some(Value::Float(100.0)));
}

#[test]
fn path_to_a_point_ends_at_the_node_beside_it() {
    let mut graph = chain("nav-to-point", CHAIN_A);
    let [_, b, c] = graph.nodes[..] else { panic!("three nodes") };

    assert_eq!(graph.native("FindPathTo", Value::Vector(Vector::new(1000.0, 5.0, 0.0))), Some(Value::Object(Some(b))));

    graph.assert_route(&[b, c], 1005.0);
}

#[test]
fn path_to_a_point_far_from_every_node_gives_none() {
    let mut graph = chain("nav-to-far-point", CHAIN_A);
    assert_eq!(graph.native("FindPathTo", Value::Vector(Vector::new(5000.0, 0.0, 0.0))), Some(Value::Object(None)));
}

#[test]
fn a_random_destination_is_a_node_of_the_graph() {
    let mut graph = chain("nav-random", CHAIN_A);
    let Some(Value::Object(Some(found))) = graph.native("FindRandomDest", Value::Map) else { panic!("no destination") };
    assert!(graph.nodes.contains(&found));
}

#[test]
fn a_flying_pawn_reaches_an_actor_within_a_thousand_units() {
    let near = Vector::new(900.0, 0.0, 0.0);
    let far = Vector::new(1100.0, 0.0, 0.0);
    let mut graph = graph("nav-actor-reach", CHAIN_A, &[near, far], &[]);
    let [near, far] = graph.nodes[..] else { panic!("two nodes") };

    assert_eq!(graph.native("actorReachable", Value::Object(Some(near))), Some(Value::Bool(true)));
    assert_eq!(graph.native("actorReachable", Value::Object(Some(far))), Some(Value::Bool(false)));
    assert_eq!(graph.native("actorReachable", Value::Object(None)), Some(Value::Bool(false)));

    assert!(graph.vm.store.set(graph.pawn, "bCanFly", Value::Bool(false)));
    assert_eq!(graph.native("actorReachable", Value::Object(Some(near))), Some(Value::Bool(false)));
}

#[test]
fn a_walking_pawn_reaches_another_pawn_in_melee_range_but_walks_nowhere_without_a_floor() {
    let mut graph = chain("nav-melee", CHAIN_A);
    graph.make_walker();
    assert!(graph.vm.store.set(graph.pawn, "MeleeRange", Value::Float(100.0)));
    let in_melee = spawn_at(&mut graph.vm, "Pawn", Vector::new(50.0, 0.0, 0.0));
    let beside = spawn_at(&mut graph.vm, "Pawn", Vector::new(100.0, 0.0, 0.0));
    let away = spawn_at(&mut graph.vm, "Pawn", Vector::new(200.0, 0.0, 0.0));

    assert_eq!(graph.native("actorReachable", Value::Object(Some(in_melee))), Some(Value::Bool(true)));
    assert_eq!(graph.native("actorReachable", Value::Object(Some(beside))), Some(Value::Bool(true)));
    assert_eq!(graph.native("actorReachable", Value::Object(Some(away))), Some(Value::Bool(false)));
}

#[test]
fn a_flying_pawn_reaches_a_point_by_its_flat_distance() {
    let mut graph = chain("nav-point-fly", CHAIN_A);
    assert_eq!(graph.native("pointReachable", Value::Vector(Vector::new(900.0, 0.0, 0.0))), Some(Value::Bool(true)));
    assert_eq!(graph.native("pointReachable", Value::Vector(Vector::new(1100.0, 0.0, 0.0))), Some(Value::Bool(false)));
    assert_eq!(graph.native("pointReachable", Value::Vector(Vector::new(0.0, 0.0, 5000.0))), Some(Value::Bool(true)));
}

#[test]
fn a_walking_pawn_without_a_floor_reaches_only_a_point_it_stands_on() {
    let mut graph = chain("nav-point-walk", CHAIN_A);
    graph.make_walker();
    assert_eq!(graph.native("pointReachable", Value::Vector(Vector::new(10.0, 0.0, 0.0))), Some(Value::Bool(true)));
    assert_eq!(graph.native("pointReachable", Value::Vector(Vector::new(200.0, 0.0, 0.0))), Some(Value::Bool(false)));
}

#[test]
fn path_node_lists_ring_every_plain_node() {
    let mut vm = load_fixture(&graph_package(), "nav-path-lists");
    let [first, second, third] = [0.0, 100.0, 200.0].map(|x| spawn_at(&mut vm, "PathNode", Vector::new(x, 0.0, 0.0)));
    let home = spawn_at(&mut vm, "HomeNode", Vector::new(300.0, 0.0, 0.0));
    let game_class = vm.find_class("Fixture", "GameInfo").unwrap();
    let game = vm.new_object(game_class, None);

    assert_eq!(vm.call_native("BuildPathNodeLists", Some(game), &mut []).unwrap(), None);

    assert_eq!(vm.store.get(first, "NextNode"), Some(Value::Object(Some(second))));
    assert_eq!(vm.store.get(second, "NextNode"), Some(Value::Object(Some(third))));
    assert_eq!(vm.store.get(third, "NextNode"), Some(Value::Object(Some(first))));
    assert_eq!(vm.store.get(home, "NextNode"), Some(Value::Object(None)));
    assert_eq!(vm.store.get(game, "PathNodeList"), Some(Value::Struct(vec![Value::Object(Some(first)), Value::Int(3)])));
}

#[test]
fn home_node_lists_ring_the_nodes_of_each_tag() {
    let mut vm = load_fixture(&graph_package(), "nav-home-lists");
    let plain = spawn_at(&mut vm, "PathNode", Vector::new(0.0, 0.0, 0.0));
    let [kitchen_one, garage, kitchen_two] = [100.0, 200.0, 300.0].map(|x| spawn_at(&mut vm, "HomeNode", Vector::new(x, 0.0, 0.0)));
    let (kitchen, garage_tag) = (vm.intern("Kitchen"), vm.intern("Garage"));
    assert!(vm.store.set(kitchen_one, "Tag", Value::Name(kitchen)));
    assert!(vm.store.set(kitchen_two, "Tag", Value::Name(kitchen)));
    assert!(vm.store.set(garage, "Tag", Value::Name(garage_tag)));
    let game_class = vm.find_class("Fixture", "GameInfo").unwrap();
    let game = vm.new_object(game_class, None);

    assert_eq!(vm.call_native("BuildHomeNodeLists", Some(game), &mut []).unwrap(), None);

    assert_eq!(vm.store.get(kitchen_one, "NextNode"), Some(Value::Object(Some(kitchen_two))));
    assert_eq!(vm.store.get(kitchen_two, "NextNode"), Some(Value::Object(Some(kitchen_one))));
    assert_eq!(vm.store.get(garage, "NextNode"), Some(Value::Object(Some(garage))));
    assert_eq!(vm.store.get(plain, "NextNode"), Some(Value::Object(None)));
    let list = |node, count| Value::Struct(vec![Value::Object(Some(node)), Value::Int(count)]);
    assert_eq!(vm.store.get(game, "HomeNodeLists"), Some(Value::Array(vec![list(kitchen_one, 2), list(garage, 1)])));
}

struct HudRefs {
    post_render: i32,
    canvas_param: i32,
    draw_tile: i32,
    draw_tile_clipped: i32,
    draw_actor: i32,
    material: i32,
    shown: i32,
    wireframe: i32,
    clear_z: i32,
}

fn native_function(package: &mut PackageBuilder, owner: i32, name: &str, params: Vec<(&str, PropertyType)>) -> i32 {
    let function = package.function(owner, name, FUNC_NATIVE);
    for (param, ty) in params {
        package.property(function, param, ty, CPF_PARM);
    }
    function
}

fn tile_params() -> Vec<(&'static str, PropertyType)> {
    let mut params = vec![("Mat", PropertyType::Object)];
    params.extend(["XL", "YL", "U", "V", "UL", "VL"].map(|name| (name, PropertyType::Float)));
    params
}

fn hud_package() -> (PackageBuilder, HudRefs) {
    let mut package = PackageBuilder::new("Engine");
    let vector = declare_vector(&mut package);
    let rotator = package.struct_type("Rotator");
    for member in ["Pitch", "Yaw", "Roll"] {
        package.property(rotator, member, PropertyType::Int, 0);
    }
    let color = package.struct_type("Color");
    for member in ["B", "G", "R", "A"] {
        package.property(color, member, PropertyType::Byte, 0);
    }

    let canvas = package.class("Canvas", None);
    for name in ["CurX", "CurY", "CurYL", "OrgX", "OrgY", "ClipX", "ClipY", "SpaceX", "FontScaleX", "FontScaleY"] {
        package.property(canvas, name, PropertyType::Float, 0);
    }
    package.property(canvas, "SizeX", PropertyType::Int, 0);
    package.property(canvas, "SizeY", PropertyType::Int, 0);
    package.property(canvas, "Style", PropertyType::Byte, 0);
    package.property(canvas, "DrawColor", PropertyType::Struct(color), 0);
    let draw_tile = native_function(&mut package, canvas, "DrawTile", tile_params());
    let draw_tile_clipped = native_function(&mut package, canvas, "DrawTileClipped", tile_params());
    let actor_params = vec![
        ("A", PropertyType::Object),
        ("Wireframe", PropertyType::Bool),
        ("ClearZ", PropertyType::Bool),
        ("DisplayFOV", PropertyType::Float),
    ];
    let draw_actor = native_function(&mut package, canvas, "DrawActor", actor_params);

    let actor = package.class("Actor", None);
    package.property(actor, "Location", PropertyType::Struct(vector), 0);
    package.property(actor, "Rotation", PropertyType::Struct(rotator), 0);
    package.property(actor, "DrawScale", PropertyType::Float, 0);
    package.property(actor, "DrawScale3D", PropertyType::Struct(vector), 0);

    package.class("Material", None);
    let controller = package.class("PlayerController", None);
    package.property(controller, "myHUD", PropertyType::Object, 0);

    let hud = package.class("HUD", None);
    let material = package.property(hud, "Mat", PropertyType::Object, 0);
    let shown = package.property(hud, "Shown", PropertyType::Object, 0);
    let wireframe = package.property(hud, "bWireframe", PropertyType::Bool, 0);
    let clear_z = package.property(hud, "bClearZ", PropertyType::Bool, 0);
    let post_render = package.function(hud, "PostRender", 0);
    let canvas_param = package.property(post_render, "C", PropertyType::Object, CPF_PARM);

    (package, HudRefs { post_render, canvas_param, draw_tile, draw_tile_clipped, draw_actor, material, shown, wireframe, clear_z })
}

fn call_on_canvas(refs: &HudRefs, function: i32) -> Script {
    Script::new().token(CONTEXT).token(LOCAL_VARIABLE).object(refs.canvas_param).token(0).token(0).token(0).token(FINAL_FUNCTION).object(function)
}

fn float_consts(mut script: Script, values: &[f32]) -> Script {
    for value in values {
        script = script.token(FLOAT_CONST).int(value.to_bits() as i32);
    }
    script
}

fn end_call(script: Script) -> Script {
    script.token(END_FUNCTION_PARMS).token(RETURN).token(NOTHING)
}

fn tile_script(refs: &HudRefs, function: i32, floats: [f32; 6]) -> Script {
    let material = call_on_canvas(refs, function).token(INSTANCE_VARIABLE).object(refs.material);
    end_call(float_consts(material, &floats))
}

struct Hud {
    vm: Vm<Arc<Loader>>,
    controller: ObjectId,
    hud: ObjectId,
    canvas: ObjectId,
}

fn hud_with(dir: &str, script: impl FnOnce(&HudRefs) -> Script) -> Hud {
    let (mut package, refs) = hud_package();
    package.script(refs.post_render, script(&refs));
    let mut vm = load_fixture(&package, dir);
    let controller_class = vm.find_class("Engine", "PlayerController").unwrap();
    let controller = vm.new_object(controller_class, None);
    assert!(vm.render_hud(controller, 640.0, 480.0).unwrap().is_empty());
    let canvas = vm.object_ids().find(|&object| vm.store.is_a(object, "Canvas")).unwrap();
    let hud_class = vm.find_class("Engine", "HUD").unwrap();
    let hud = vm.new_object(hud_class, None);
    assert!(vm.store.set(controller, "myHUD", Value::Object(Some(hud))));
    let material_class = vm.find_class("Engine", "Material").unwrap();
    let material = vm.new_object(material_class, Some("Smiley"));
    assert!(vm.store.set(hud, "Mat", Value::Object(Some(material))));
    assert!(vm.store.set(canvas, "OrgX", Value::Float(5.0)));
    assert!(vm.store.set(canvas, "OrgY", Value::Float(7.0)));
    assert!(vm.store.set(canvas, "CurY", Value::Float(20.0)));
    assert!(vm.store.set(canvas, "Style", Value::Byte(3)));
    let color = Value::Struct(vec![Value::Byte(30), Value::Byte(20), Value::Byte(10), Value::Byte(40)]);
    assert!(vm.store.set(canvas, "DrawColor", color));
    Hud { vm, controller, hud, canvas }
}

#[test]
fn render_hud_sizes_the_canvas_and_returns_nothing_without_a_hud() {
    let mut hud = hud_with("canvas-size", |refs| end_call(call_on_canvas(refs, refs.draw_tile)));
    assert!(hud.vm.store.set(hud.controller, "myHUD", Value::Object(None)));

    assert!(hud.vm.render_hud(hud.controller, 800.0, 600.0).unwrap().is_empty());

    assert_eq!(hud.vm.store.get(hud.canvas, "SizeX"), Some(Value::Int(800)));
    assert_eq!(hud.vm.store.get(hud.canvas, "SizeY"), Some(Value::Int(600)));
    assert_eq!(hud.vm.store.get(hud.canvas, "ClipX"), Some(Value::Float(800.0)));
    assert_eq!(hud.vm.store.get(hud.canvas, "ClipY"), Some(Value::Float(600.0)));
}

#[test]
fn draw_tile_from_a_hud_emits_a_tile_at_origin_plus_pen() {
    let mut hud = hud_with("canvas-tile", |refs| tile_script(refs, refs.draw_tile, [64.0, 32.0, 1.0, 2.0, 3.0, 4.0]));
    assert!(hud.vm.store.set(hud.canvas, "CurX", Value::Float(10.0)));

    let items = hud.vm.render_hud(hud.controller, 640.0, 480.0).unwrap();

    assert_eq!(items, [CanvasItem::Tile {
        material: Arc::from("Transient.Smiley"),
        position: [15.0, 27.0],
        size: [64.0, 32.0],
        uv: [1.0, 2.0, 3.0, 4.0],
        color: [10, 20, 30, 40],
        style: 3,
    }]);
    assert_eq!(hud.vm.store.get(hud.canvas, "CurX"), Some(Value::Float(74.0)));
    assert_eq!(hud.vm.store.get(hud.canvas, "CurYL"), Some(Value::Float(32.0)));
}

#[test]
fn draw_tile_in_style_none_emits_nothing_but_still_moves_the_pen() {
    let mut hud = hud_with("canvas-style-none", |refs| tile_script(refs, refs.draw_tile, [64.0, 32.0, 0.0, 0.0, 64.0, 32.0]));
    assert!(hud.vm.store.set(hud.canvas, "CurX", Value::Float(10.0)));
    assert!(hud.vm.store.set(hud.canvas, "Style", Value::Byte(0)));

    assert!(hud.vm.render_hud(hud.controller, 640.0, 480.0).unwrap().is_empty());

    assert_eq!(hud.vm.store.get(hud.canvas, "CurX"), Some(Value::Float(74.0)));
}

#[test]
fn draw_tile_clipped_from_a_hud_cuts_the_tile_at_the_clip_edge() {
    let mut hud = hud_with("canvas-tile-clipped", |refs| tile_script(refs, refs.draw_tile_clipped, [64.0, 32.0, 0.0, 0.0, 64.0, 32.0]));
    assert!(hud.vm.store.set(hud.canvas, "CurX", Value::Float(630.0)));

    let items = hud.vm.render_hud(hud.controller, 640.0, 480.0).unwrap();

    assert_eq!(items, [CanvasItem::Tile {
        material: Arc::from("Transient.Smiley"),
        position: [635.0, 27.0],
        size: [10.0, 32.0],
        uv: [0.0, 0.0, 10.0, 32.0],
        color: [10, 20, 30, 40],
        style: 3,
    }]);
    assert_eq!(hud.vm.store.get(hud.canvas, "CurX"), Some(Value::Float(640.0)));
}

#[test]
fn draw_actor_from_a_hud_records_the_actor_as_placed_at_the_call() {
    let mut hud = hud_with("canvas-actor", |refs| {
        let args = call_on_canvas(refs, refs.draw_actor)
            .token(INSTANCE_VARIABLE)
            .object(refs.shown)
            .token(INSTANCE_VARIABLE)
            .object(refs.wireframe)
            .token(INSTANCE_VARIABLE)
            .object(refs.clear_z);
        end_call(float_consts(args, &[90.0]))
    });
    let actor_class = hud.vm.find_class("Engine", "Actor").unwrap();
    let actor = hud.vm.new_object(actor_class, None);
    assert!(hud.vm.store.set(actor, "Location", Value::Vector(Vector::new(1.0, 2.0, 3.0))));
    assert!(hud.vm.store.set(actor, "Rotation", Value::Rotator(Rotator::new(4, 5, 6))));
    assert!(hud.vm.store.set(actor, "DrawScale", Value::Float(2.0)));
    assert!(hud.vm.store.set(actor, "DrawScale3D", Value::Vector(Vector::new(1.0, 2.0, 3.0))));
    assert!(hud.vm.store.set(hud.hud, "Shown", Value::Object(Some(actor))));
    assert!(hud.vm.store.set(hud.hud, "bClearZ", Value::Bool(true)));

    let items = hud.vm.render_hud(hud.controller, 640.0, 480.0).unwrap();

    assert_eq!(items, [CanvasItem::Actor {
        actor,
        clear_z: true,
        fov: 90.0,
        location: Vector::new(1.0, 2.0, 3.0),
        rotation: Rotator::new(4, 5, 6),
        scale: Vector::new(2.0, 4.0, 6.0),
    }]);
}

fn canvas_alone(dir: &str) -> (Vm<Arc<Loader>>, ObjectId, ObjectId) {
    let (package, _refs) = hud_package();
    let mut vm = load_fixture(&package, dir);
    let canvas_class = vm.find_class("Engine", "Canvas").unwrap();
    let canvas = vm.new_object(canvas_class, None);
    let material_class = vm.find_class("Engine", "Material").unwrap();
    let material = vm.new_object(material_class, None);
    (vm, canvas, material)
}

fn tile_args(material: Option<ObjectId>, size: [f32; 2], uv: [f32; 4]) -> Vec<Value> {
    let floats = size.into_iter().chain(uv).map(Value::Float);
    std::iter::once(Value::Object(material)).chain(floats).collect()
}

fn set_floats(vm: &mut Vm<Arc<Loader>>, object: ObjectId, values: &[(&str, f32)]) {
    for &(name, value) in values {
        assert!(vm.store.set(object, name, Value::Float(value)));
    }
}

fn assert_floats(vm: &Vm<Arc<Loader>>, object: ObjectId, values: &[(&str, f32)]) {
    for &(name, value) in values {
        assert_eq!(vm.store.get(object, name), Some(Value::Float(value)), "{name}");
    }
}

#[test]
fn draw_tile_moves_the_pen_across_and_keeps_its_line() {
    let (mut vm, canvas, material) = canvas_alone("canvas-pen");
    set_floats(&mut vm, canvas, &[("CurX", 10.0), ("CurY", 20.0), ("CurYL", 99.0)]);

    let drawn = vm.call_native("DrawTile", Some(canvas), &mut tile_args(Some(material), [64.0, 32.0], [0.0, 0.0, 64.0, 32.0])).unwrap();

    assert_eq!(drawn, None);
    assert_floats(&vm, canvas, &[("CurX", 74.0), ("CurY", 20.0), ("CurYL", 32.0)]);
}

#[test]
fn draw_tile_without_a_material_or_a_canvas_does_nothing() {
    let (mut vm, canvas, material) = canvas_alone("canvas-no-material");
    set_floats(&mut vm, canvas, &[("CurX", 10.0), ("CurYL", 5.0)]);

    vm.call_native("DrawTile", Some(canvas), &mut tile_args(None, [64.0, 32.0], [0.0; 4])).unwrap();
    vm.call_native("DrawTileClipped", Some(canvas), &mut tile_args(None, [64.0, 32.0], [0.0; 4])).unwrap();
    assert_eq!(vm.call_native("DrawTile", None, &mut tile_args(Some(material), [64.0, 32.0], [0.0; 4])).unwrap(), None);

    assert_floats(&vm, canvas, &[("CurX", 10.0), ("CurYL", 5.0)]);
}

#[test]
fn clipped_tile_moves_the_pen_by_what_was_kept_plus_spacing() {
    let (mut vm, canvas, material) = canvas_alone("canvas-clipped-pen");
    set_floats(&mut vm, canvas, &[("CurX", 90.0), ("ClipX", 100.0), ("ClipY", 100.0), ("SpaceX", 3.0), ("CurYL", 4.0)]);

    vm.call_native("DrawTileClipped", Some(canvas), &mut tile_args(Some(material), [20.0, 10.0], [0.0, 0.0, 64.0, 32.0])).unwrap();

    assert_floats(&vm, canvas, &[("CurX", 103.0), ("CurYL", 10.0)]);
}

#[test]
fn clipped_tile_keeps_a_taller_line_height() {
    let (mut vm, canvas, material) = canvas_alone("canvas-clipped-line");
    set_floats(&mut vm, canvas, &[("ClipX", 100.0), ("ClipY", 100.0), ("CurYL", 25.0)]);

    vm.call_native("DrawTileClipped", Some(canvas), &mut tile_args(Some(material), [20.0, 10.0], [0.0, 0.0, 64.0, 32.0])).unwrap();

    assert_floats(&vm, canvas, &[("CurX", 20.0), ("CurYL", 25.0)]);
}

#[test]
fn clipped_tile_outside_the_box_leaves_the_pen_alone() {
    let (mut vm, canvas, material) = canvas_alone("canvas-clipped-out");
    set_floats(&mut vm, canvas, &[("CurX", 120.0), ("ClipX", 100.0), ("ClipY", 100.0), ("CurYL", 4.0)]);

    vm.call_native("DrawTileClipped", Some(canvas), &mut tile_args(Some(material), [20.0, 10.0], [0.0, 0.0, 64.0, 32.0])).unwrap();

    assert_floats(&vm, canvas, &[("CurX", 120.0), ("CurYL", 4.0)]);
}

#[test]
fn draw_text_without_a_font_advances_by_eight_by_sixteen_per_character() {
    let (mut vm, canvas, _material) = canvas_alone("canvas-text");
    set_floats(&mut vm, canvas, &[("CurX", 10.0), ("CurY", 20.0)]);

    vm.call_native("DrawText", Some(canvas), &mut [Value::Str("Hello".to_owned()), Value::Bool(false)]).unwrap();

    assert_floats(&vm, canvas, &[("CurX", 50.0), ("CurY", 20.0), ("CurYL", 16.0)]);
}

#[test]
fn draw_text_with_a_line_break_returns_the_pen_and_drops_a_line() {
    let (mut vm, canvas, _material) = canvas_alone("canvas-text-break");
    set_floats(&mut vm, canvas, &[("CurX", 10.0), ("CurY", 20.0)]);

    vm.call_native("DrawText", Some(canvas), &mut [Value::Str("Hello".to_owned()), Value::Bool(true)]).unwrap();

    assert_floats(&vm, canvas, &[("CurX", 0.0), ("CurY", 36.0), ("CurYL", 16.0)]);
}

#[test]
fn draw_text_scales_its_advance_by_the_font_scale() {
    let (mut vm, canvas, _material) = canvas_alone("canvas-text-scale");
    set_floats(&mut vm, canvas, &[("FontScaleX", 2.0), ("FontScaleY", 1.5)]);

    vm.call_native("DrawText", Some(canvas), &mut [Value::Str("Hello".to_owned()), Value::Bool(false)]).unwrap();

    assert_floats(&vm, canvas, &[("CurX", 80.0), ("CurYL", 24.0)]);
}

#[test]
fn string_sizes_come_back_through_the_out_arguments() {
    let (mut vm, canvas, _material) = canvas_alone("canvas-text-size");
    for native in ["StrLen", "TextSize"] {
        let mut args = [Value::Str("abc".to_owned()), Value::Float(0.0), Value::Float(0.0)];
        assert_eq!(vm.call_native(native, Some(canvas), &mut args).unwrap(), None);
        assert_eq!(args[1], Value::Float(24.0), "{native}");
        assert_eq!(args[2], Value::Float(16.0), "{native}");
    }

    set_floats(&mut vm, canvas, &[("FontScaleX", 2.0), ("FontScaleY", 1.5)]);
    let mut args = [Value::Str("abc".to_owned()), Value::Float(0.0), Value::Float(0.0)];
    vm.call_native("StrLen", Some(canvas), &mut args).unwrap();
    assert_eq!(args[1], Value::Float(48.0));
    assert_eq!(args[2], Value::Float(24.0));
}
