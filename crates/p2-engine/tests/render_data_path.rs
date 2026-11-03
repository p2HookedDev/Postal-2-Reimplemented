mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::camera::{Camera, Camera3d, PerspectiveProjection, Projection, RenderTarget};
use bevy::image::Image;
use bevy::pbr::MeshMaterial3d;
use bevy::render::render_resource::TextureFormat;
use bevy::MinimalPlugins;
use bevy::app::{App, Last, Update};
use bevy::asset::{AssetApp, AssetPlugin, Assets, Handle, RenderAssetUsages};
use bevy::camera::primitives::{Aabb, MeshAabb};
use bevy::camera::visibility::{DynamicSkinnedMeshBounds, NoFrustumCulling, Visibility, VisibilityPlugin};
use bevy::ecs::prelude::{ChildOf, Changed, Entity, IntoScheduleConfigs, Query, Ref, Res, ResMut, Resource, With};
use bevy::ecs::change_detection::DetectChanges;
use bevy::math::{Mat4, Vec3};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::StandardMaterial;
use bevy::time::{Time, TimeUpdateStrategy};
use bevy::transform::TransformPlugin;
use bevy::transform::components::{GlobalTransform, Transform, TransformTreeChanged};
use p2_assets::{AnalogTrack, AnimSequence, Bone, Influence, MeshAnimation, NamedBone, SkelMeshSection, SkeletalMesh, Wedge};
use p2_bevy_assets::{bone_matrices, skeletal_mesh_to_meshes, skin_bind_transforms, skin_inverse_bindposes, unreal_to_bevy};
use p2_engine::animation::{Animated, AnimatedMesh, AnimationPlugin};
use p2_engine::mesh_view::{ActorModel, FollowActor, SectionMaterial, SectionView, Skin, SkinJoint, spawn_actor_sections};
use p2_engine::play::FrameTimes;
use p2_vm::ObjectId;

const BONES: usize = 60;
const ANIMATED: usize = 30;

fn pawn_mesh() -> SkeletalMesh {
    let parents: Vec<usize> = (0..BONES).map(|i| if i == 0 { 0 } else { (i - 1) / 2 }).collect();
    let bones: Vec<Bone> = (0..BONES)
        .map(|i| Bone {
            name: i,
            parent: parents[i],
            num_children: 0,
            orientation: [0.0, 0.0, 0.0, 1.0],
            position: if i == 0 { [0.0; 3] } else { [10.0, 2.0 * (i % 3) as f32, 3.0] },
        })
        .collect();
    let local: Vec<_> = bones.iter().map(|b| (b.orientation, b.position)).collect();
    let bind = bone_matrices(&parents, &local);
    let mut positions = Vec::new();
    let mut influences = Vec::new();
    for (bone, matrix) in bind.iter().enumerate() {
        let at = matrix.w_axis.truncate();
        for offset in [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 4.0]] {
            positions.push((at + Vec3::from(offset)).to_array());
            influences.push(Influence { bone: bone as u16, weight: 1.0 });
        }
    }
    let influence_starts = (0..=influences.len() as u32).collect();
    let wedges = (0..positions.len() as u32).map(|point| Wedge { point, uv: [0.0, 0.0] }).collect();
    let indices: Vec<u16> = (0..BONES as u16).flat_map(|b| [4 * b, 4 * b + 1, 4 * b + 2, 4 * b, 4 * b + 2, 4 * b + 3]).collect();
    let faces = (indices.len() / 3) as u16;
    SkeletalMesh {
        materials: vec![0, 0],
        scale: p2_math::Vector::new(1.0, 1.0, 1.0),
        origin: p2_math::Vector::ZERO,
        rot_origin: p2_math::Rotator::ZERO,
        positions,
        influences,
        influence_starts,
        wedges,
        indices,
        sections: vec![
            SkelMeshSection { material: 0, first_face: 0, num_faces: faces / 2 },
            SkelMeshSection { material: 1, first_face: faces / 2, num_faces: faces - faces / 2 },
        ],
        bones,
        default_animation: 0,
        attach_aliases: Vec::new(),
    }
}

fn pawn_animation(mesh: &SkeletalMesh) -> MeshAnimation {
    let tracks = mesh
        .bones
        .iter()
        .enumerate()
        .map(|(i, bone)| {
            let rotations = if i >= ANIMATED { vec![bone.orientation, [0.0, 0.383, 0.0, 0.924]] } else { vec![bone.orientation] };
            let times = if i >= ANIMATED { vec![0.0, 15.0] } else { vec![0.0] };
            AnalogTrack { flags: 0, rotations, positions: vec![bone.position], times, times_sorted: true }
        })
        .collect();
    MeshAnimation {
        version: 118,
        bones: mesh.bones.iter().map(|b| NamedBone { name: b.name, flags: 0, parent: b.parent as i32 }).collect(),
        sequences: vec![AnimSequence {
            name: 0,
            groups: Vec::new(),
            start_frame: 0,
            num_frames: 30,
            rate: 30.0,
            notifies: Vec::new(),
            root_speed: p2_math::Vector::ZERO,
            track_time: 30.0,
            start_bone: 0,
            flags: 0,
            bone_indices: Vec::new(),
            tracks,
            root_track: AnalogTrack::default(),
        }],
    }
}

fn triangle() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
    mesh
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Layout {
    Old,
    New,
}

#[derive(Clone, Copy)]
struct Shape {
    pawns: usize,
    posed: usize,
    moving: usize,
    gone: usize,
    statics: usize,
    particles: usize,
    others: usize,
}

#[derive(Resource, Default)]
struct OldPoses {
    items: Vec<AnimatedMesh>,
    bones: HashMap<ObjectId, Vec<Mat4>>,
    changed: HashSet<ObjectId>,
}

fn old_animate(time: Res<Time>, mut poses: ResMut<OldPoses>) {
    let poses = &mut *poses;
    poses.changed.clear();
    for item in &poses.items {
        let Some(actor) = item.actor else { continue };
        let rate = item.animation.sequences[item.sequence].rate;
        poses.bones.insert(actor, item.bone_pose(time.elapsed_secs() * rate));
        poses.changed.insert(actor);
    }
}

fn old_pose_joints(poses: Res<OldPoses>, mut joints: Query<(Ref<SkinJoint>, &mut Transform)>) {
    for (joint, mut transform) in &mut joints {
        if !(joint.is_added() || poses.changed.contains(&joint.actor)) {
            continue;
        }
        if let Some(&bone) = poses.bones.get(&joint.actor).and_then(|pose| pose.get(joint.bone)) {
            *transform = unreal_to_bevy(bone);
        }
    }
}

#[derive(Resource)]
struct Walkers(HashSet<ObjectId>);

fn walk(time: Res<Time>, walkers: Res<Walkers>, mut models: Query<(&FollowActor, &mut Transform), With<SkinnedMesh>>) {
    let step = time.elapsed_secs();
    for (follow, mut transform) in &mut models {
        if walkers.0.contains(&follow.actor) {
            transform.translation.z = step * 100.0;
        }
    }
}

fn pawns(count: usize) -> HashSet<ObjectId> {
    (0..count as u32).map(ObjectId::from_index).collect()
}

#[derive(Resource, Default, Clone, Copy, Debug)]
struct Counts {
    transforms: usize,
    joints: usize,
    globals: usize,
    bounds: usize,
    dirty_nodes: usize,
    static_root_dirty: bool,
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system: each parameter is state it reads or changes")]
fn count(
    mut counts: ResMut<Counts>,
    transforms: Query<(), Changed<Transform>>,
    joints: Query<(), (Changed<Transform>, With<SkinJoint>)>,
    globals: Query<(), Changed<GlobalTransform>>,
    bounds: Query<(), Changed<Aabb>>,
    dirty: Query<(), Changed<TransformTreeChanged>>,
    trees: Query<Ref<TransformTreeChanged>>,
    root: Res<StaticRoot>,
) {
    *counts = Counts {
        transforms: transforms.iter().count(),
        joints: joints.iter().count(),
        globals: globals.iter().count(),
        bounds: bounds.iter().count(),
        dirty_nodes: dirty.iter().count(),
        static_root_dirty: trees.get(root.0).is_ok_and(|tree| tree.is_changed()),
    };
}

#[derive(Resource)]
struct StaticRoot(Entity);

fn world(layout: Layout, shape: Shape, gpu: bool) -> App {
    let mut app = App::new();
    if gpu {
        common::headless_renderer(&mut app);
    } else {
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), TransformPlugin, VisibilityPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_asset::<SkinnedMeshInverseBindposes>();
    }
    app.init_resource::<FrameTimes>()
        .init_resource::<Counts>()
        .insert_resource(Walkers(pawns(shape.moving)))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .add_systems(Last, count);
    match layout {
        Layout::Old => {
            app.init_resource::<OldPoses>().add_systems(Update, (walk, old_animate, old_pose_joints).chain());
        }
        Layout::New => {
            app.init_resource::<p2_engine::play::Play>().add_plugins(AnimationPlugin).add_systems(Update, walk.before(p2_engine::mesh_view::MeshViewSystems::Camera));
        }
    }
    let world = app.world_mut();
    let pawn = Arc::new(pawn_mesh());
    let animation = Arc::new(pawn_animation(&pawn));
    let sections: Vec<Handle<Mesh>> =
        skeletal_mesh_to_meshes(&pawn, true).into_iter().map(|(mesh, _)| world.resource_mut::<Assets<Mesh>>().add(mesh)).collect();
    let inverse_bindposes = world.resource_mut::<Assets<SkinnedMeshInverseBindposes>>().add(SkinnedMeshInverseBindposes::from(skin_inverse_bindposes(&pawn)));
    let skin = Skin { inverse_bindposes, bind: skin_bind_transforms(&pawn).into() };
    let small = world.resource_mut::<Assets<Mesh>>().add(triangle());
    let center = Vec3::new(5000.0, 0.0, 5000.0);
    let root = world.spawn((Transform::from_translation(-center), Visibility::default())).id();
    world.insert_resource(StaticRoot(root));
    let material = MeshMaterial3d(Handle::<StandardMaterial>::default());
    for i in 0..shape.statics {
        world.spawn((Mesh3d(small.clone()), material.clone(), Transform::from_xyz((i % 100) as f32 * 100.0, 0.0, (i / 100) as f32 * 100.0), ChildOf(root)));
    }
    if gpu {
        let target = world.resource_mut::<Assets<Image>>().add(Image::new_target_texture(256, 256, TextureFormat::Rgba8UnormSrgb, None));
        world.spawn((
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(target.into()),
            Projection::Perspective(PerspectiveProjection { far: 100_000.0, ..Default::default() }),
            Transform::from_xyz(7500.0, 6000.0, 20_000.0).looking_at(Vec3::new(7500.0, 0.0, 0.0), Vec3::Y),
        ));
    }
    let particles = world.spawn((Transform::default(), Visibility::default())).id();
    for _ in 0..shape.particles {
        world.spawn((Mesh3d(small.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling, ChildOf(particles)));
    }
    for i in 0..shape.others {
        world.spawn(Transform::from_xyz(i as f32, 0.0, 0.0));
    }
    let heads = match layout {
        Layout::Old => root,
        Layout::New => world.spawn((Transform::from_translation(-center), Visibility::default())).id(),
    };
    let spawned = shape.pawns + if layout == Layout::Old { shape.gone } else { 0 };
    let mut items = Vec::new();
    for index in 0..spawned {
        let actor = ObjectId::from_index(index as u32);
        let follow = FollowActor { actor, scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: Some(([1.0; 3].into(), [0.0; 3].into(), [0; 3].into())) };
        let parent = match layout {
            Layout::Old => root,
            Layout::New => world.spawn((ActorModel(actor), Transform::from_translation(-center), Visibility::default())).id(),
        };
        let views = sections.iter().map(|mesh| SectionView { skin: Some(skin.clone()), ..SectionView::textured(mesh.clone(), None) }).collect();
        let placed = Transform::from_xyz(index as f32 * 50.0, 0.0, 0.0);
        let mut commands = world.commands();
        spawn_actor_sections(&mut commands, parent, views, placed, (follow.actor, follow), |_: &SectionView| SectionMaterial::Standard(Handle::<StandardMaterial>::default()), None, ());
        world.flush();
        world.spawn((Mesh3d(small.clone()), material.clone(), placed, ChildOf(heads)));
        if index < shape.posed {
            let mut item = AnimatedMesh::new(sections.clone(), Arc::clone(&pawn), Arc::clone(&animation), 0, |i| format!("b{i}"), |i| format!("b{i}"));
            item.actor = Some(actor);
            items.push(item);
        }
    }
    if layout == Layout::Old {
        let alive = pawns(shape.pawns);
        let skinned: Vec<(Entity, bool)> =
            world.query::<(Entity, &FollowActor, &SkinnedMesh)>().iter(world).map(|(e, f, _)| (e, alive.contains(&f.actor))).collect();
        for (entity, alive) in skinned {
            world.entity_mut(entity).insert(DynamicSkinnedMeshBounds);
            if !alive {
                world.entity_mut(entity).insert(Visibility::Hidden);
            }
        }
        world.resource_mut::<OldPoses>().items = items;
    } else {
        world.resource_mut::<Animated>().0 = items;
    }
    app.finish();
    app.cleanup();
    app.update();
    app
}

fn frame(app: &mut App) -> Counts {
    app.update();
    *app.world().resource::<Counts>()
}

const SMALL: Shape = Shape { pawns: 12, posed: 8, moving: 3, gone: 4, statics: 50, particles: 10, others: 10 };

#[test]
fn joints_are_written_only_where_bones_move() {
    for (layout, per_pawn) in [(Layout::Old, BONES), (Layout::New, BONES - ANIMATED)] {
        let mut app = world(layout, SMALL, false);
        frame(&mut app);
        for _ in 0..5 {
            let counts = frame(&mut app);
            assert_eq!(counts.joints, SMALL.posed * per_pawn, "{layout:?}");
        }
    }
}

#[test]
fn skinned_bounds_follow_new_poses_only() {
    let (mut old, mut new) = (world(Layout::Old, SMALL, false), world(Layout::New, SMALL, false));
    for _ in 0..4 {
        let (old_counts, new_counts) = (frame(&mut old), frame(&mut new));
        assert_eq!(old_counts.bounds, 2 * (SMALL.pawns + SMALL.gone));
        assert_eq!(new_counts.bounds, 2 * SMALL.posed);
    }
    let alive = pawns(SMALL.pawns);
    let bounds = |app: &mut App| -> Vec<(ObjectId, Aabb)> {
        let world = app.world_mut();
        let mut found: Vec<(ObjectId, Aabb)> =
            world.query::<(&FollowActor, &Aabb)>().iter(world).filter(|(f, _)| alive.contains(&f.actor)).map(|(f, a)| (f.actor, *a)).collect();
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.center.x.total_cmp(&b.1.center.x)));
        found
    };
    let (old_bounds, new_bounds) = (bounds(&mut old), bounds(&mut new));
    assert_eq!(old_bounds.len(), new_bounds.len());
    for ((index, a), (_, b)) in old_bounds.iter().zip(&new_bounds) {
        assert!(a.center.abs_diff_eq(b.center, 1e-3) && a.half_extents.abs_diff_eq(b.half_extents, 1e-3), "pawn {index:?}: {a:?} vs {b:?}");
    }
    let bind = skeletal_mesh_to_meshes(&pawn_mesh(), true)[0].0.get_aabb();
    assert!(new_bounds.iter().any(|(_, a)| Some(*a) != bind));
}

#[test]
fn moving_pawns_leave_the_static_scene_out() {
    for (layout, dirty) in [(Layout::Old, true), (Layout::New, false)] {
        let mut app = world(layout, SMALL, false);
        for _ in 0..3 {
            assert_eq!(frame(&mut app).static_root_dirty, dirty, "{layout:?}");
        }
    }
}

#[test]
#[ignore = "benchmark"]
fn benchmark_monday_shape() {
    benchmark(false);
}

#[test]
#[ignore = "benchmark, needs a GPU"]
fn benchmark_monday_shape_gpu() {
    benchmark(true);
}

fn benchmark(gpu: bool) {
    let shapes = [
        ("every pawn seen", Shape { pawns: 500, posed: 500, moving: 100, gone: 0, statics: 3000, particles: 2000, others: 2000 }),
        ("100 pawns seen", Shape { pawns: 500, posed: 100, moving: 100, gone: 0, statics: 3000, particles: 2000, others: 2000 }),
        ("100 seen, 250 destroyed", Shape { pawns: 500, posed: 100, moving: 100, gone: 250, statics: 3000, particles: 2000, others: 2000 }),
    ];
    for (name, shape) in shapes {
        let mut apps = [world(Layout::Old, shape, gpu), world(Layout::New, shape, gpu)];
        for app in &mut apps {
            for _ in 0..if gpu { 300 } else { 30 } {
                app.update();
            }
        }
        let mut times = [Vec::new(), Vec::new()];
        let mut counts = [Counts::default(); 2];
        for _ in 0..12 {
            for (app, (times, counts)) in apps.iter_mut().zip(times.iter_mut().zip(counts.iter_mut())) {
                for _ in 0..20 {
                    let started = Instant::now();
                    app.update();
                    times.push(started.elapsed());
                }
                *counts = *app.world().resource::<Counts>();
            }
        }
        for ((layout, app), (times, counts)) in [Layout::Old, Layout::New].iter().zip(&mut apps).zip(times.iter_mut().zip(counts)) {
            times.sort();
            let world = app.world_mut();
            let entities = world.query::<Entity>().iter(world).count();
            let median = times[times.len() / 2].as_secs_f64() * 1000.0;
            let low = times[times.len() / 10].as_secs_f64() * 1000.0;
            eprintln!(
                "{name:24} {layout:?}: {entities} entities, frame median {median:.3} ms (p10 {low:.3}); per frame: {} transforms written ({} joints), {} global transforms, {} bounds, {} dirty tree nodes",
                counts.transforms, counts.joints, counts.globals, counts.bounds, counts.dirty_nodes
            );
        }
    }
}
