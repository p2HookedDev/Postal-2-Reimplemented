use super::*;
use p2_assets::pose::{LocalBone, bone_frames, mesh_to_world};
use p2_assets::{MeshAnimation, read_mesh_animation, read_skeletal_mesh};
use p2_package::Package;
use p2_physics::World;
use p2_physics::{RagdollParams, Shape};
use super::rig::build;

const GRAVITY: [f32; 3] = [0.0, 0.0, -950.0];
const STANDING_HEIGHT: f32 = 72.0;
const CROUCH_HEIGHT: f32 = 52.0;
const FLOOR_GAP: f32 = 2.0;

struct Rig {
    mesh: PosedMesh,
    animation: MeshAnimation,
    names: Vec<String>,
    tracks: Vec<Option<usize>>,
    asset: KarmaAsset,
}

fn rig() -> Option<Rig> {
    let dir = std::path::PathBuf::from(std::env::var_os("P2_GAME_DIR")?);
    let data = std::fs::read(dir.join("Animations").join("Characters.ukx")).ok()?;
    let package = Package::parse(&data).ok()?;
    let export = |name: &str| package.exports.iter().find(|e| package.name(e.object_name) == name);
    let read = read_skeletal_mesh(&package, &data, export("Avg_M_SS_Shorts_PB_D")?).ok()?;
    let lower = |index: usize| package.name(index).to_ascii_lowercase();
    let animation = package
        .exports
        .iter()
        .filter(|e| package.object_name(e.class_index) == Some("MeshAnimation"))
        .filter_map(|e| read_mesh_animation(&package, &data, e).ok())
        .find(|a| a.sequences.iter().any(|s| lower(s.name) == "c_beg1"))?;
    let names: Vec<String> = read.bones.iter().map(|b| lower(b.name)).collect();
    let parents = read.bones.iter().map(|b| b.parent).collect();
    let reference = read.bones.iter().enumerate().map(|(bone, b)| crate::bones::reference_frame(bone, b.orientation, b.position)).collect();
    let tracks = names.iter().map(|name| animation.bones.iter().position(|a| lower(a.name) == *name)).collect();
    let sequences = animation.sequences.iter().map(|s| lower(s.name)).collect();
    let mesh = PosedMesh { mesh: read, names, parents, aliases: Vec::new(), reference };
    let text = std::fs::read_to_string(dir.join("KarmaData").join("Avg_Dude.ka")).ok()?;
    let asset = read_karma_assets(&text).ok()?.into_iter().find(|a| a.id.eq_ignore_ascii_case("avg_dude_skel"))?;
    Some(Rig { mesh, animation, names: sequences, tracks, asset })
}

fn params(rig: &Rig, sequence: &str, fraction: f32, crouched: bool) -> Option<RagdollParams> {
    let index = rig.names.iter().position(|n| n == sequence)?;
    let frames = rig.animation.sequences[index].num_frames.max(1) as f32;
    let pose = rig.animation.pose_at(index, fraction * frames);
    let local: Vec<LocalBone> = rig.tracks.iter().zip(&rig.mesh.mesh.bones).map(|(&track, bone)| track.and_then(|t| pose.get(t)).unwrap_or((bone.orientation, bone.position))).collect();
    let bones = bone_frames(&rig.mesh.parents, &local, |_| 1.0);
    let (height, pre_pivot) = if crouched { (CROUCH_HEIGHT, STANDING_HEIGHT - CROUCH_HEIGHT) } else { (STANDING_HEIGHT, 0.0) };
    let location: Vector = [0.0, 0.0, height + FLOOR_GAP].into();
    let world = mesh_to_world(location, p2_math::Rotator::default(), [1.0; 3].into(), [0.0, 0.0, pre_pivot].into(), rig.mesh.mesh.scale, rig.mesh.mesh.origin, rig.mesh.mesh.rot_origin);
    let frames: Vec<Frame> = bones.iter().map(|frame| world.then(frame)).collect();
    let built = build(&rig.asset, &rig.mesh, &frames, [0.0; 3].into(), &[])?;
    Some(RagdollParams { parts: built.parts, joints: built.joint_params, linear_damping: 0.2, angular_damping: 0.2, friction: CORPSE_FRICTION, restitution: 0.0, asleep: false })
}

const CELL: f32 = 64.0;
const CELLS: i32 = 60;

fn bump(x: f32, y: f32, amplitude: f32) -> f32 {
    amplitude * ((x / 90.0).sin() + (y / 70.0).cos() + (x / 37.0 + y / 53.0).sin()) / 3.0
}

fn ground_height(x: f32, y: f32, amplitude: f32) -> f32 {
    let (cx, cy) = (x / CELL, y / CELL);
    let (i, j) = (cx.floor(), cy.floor());
    let (u, v) = (cx - i, cy - j);
    let corner = |a: f32, b: f32| bump((i + a) * CELL, (j + b) * CELL, amplitude);
    if u >= v {
        corner(0.0, 0.0) + (corner(1.0, 0.0) - corner(0.0, 0.0)) * u + (corner(1.0, 1.0) - corner(1.0, 0.0)) * v
    } else {
        corner(0.0, 0.0) + (corner(0.0, 1.0) - corner(0.0, 0.0)) * v + (corner(1.0, 1.0) - corner(0.0, 1.0)) * u
    }
}

fn ground(amplitude: f32) -> World {
    let mut world = World::new(GRAVITY.into());
    let side = CELLS * 2 + 1;
    let mut vertices = Vec::new();
    for j in -CELLS..=CELLS {
        for i in -CELLS..=CELLS {
            let (x, y) = (i as f32 * CELL, j as f32 * CELL);
            vertices.push([x, y, bump(x, y, amplitude)]);
        }
    }
    let at = |i: i32, j: i32| (j * side + i) as u32;
    let mut triangles = Vec::new();
    for j in 0..side - 1 {
        for i in 0..side - 1 {
            triangles.push([at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
            triangles.push([at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
        }
    }
    world.add_static_mesh(&vertices, triangles).unwrap();
    world
}

fn lowest_axis_end(world: &World, bodies: &[BodyId], params: &RagdollParams, amplitude: f32) -> f32 {
    let mut lowest = f32::MAX;
    for (&body, part) in bodies.iter().zip(&params.parts) {
        let (location, rotation) = world.pose(body).unwrap();
        let frame = Frame::from_quat(rotation, 1.0, location.into());
        for shape in &part.shapes {
            if let Shape::Capsule(a, b, _) = shape {
                for end in [*a, *b] {
                    let p = frame.transform_point(end.into());
                    lowest = lowest.min(p[2] - ground_height(p[0], p[1], amplitude));
                }
            }
        }
    }
    lowest
}

#[test]
fn real_skeleton_hit_hard_keeps_its_limbs_above_the_ground() {
    let Some(rig) = rig() else { return };
    let mut seed = 12345u32;
    let mut unit = move || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        ((seed >> 16) & 0x7fff) as f32 / 16383.5 - 1.0
    };
    let (mut limbs_under, mut cases, mut worst) = (0, 0, f32::MAX);
    for sequence in ["c_beg1", "c_death", "s_fall_fore"] {
        for fraction in [0.25, 0.75] {
            for amplitude in [0.0, 15.0, 40.0] {
                for _ in 0..6 {
                    let params = params(&rig, sequence, fraction, sequence.starts_with("c_")).expect("a sequence of this skeleton");
                    let mut world = ground(amplitude);
                    let bodies = world.add_ragdoll(&params);
                    for &body in &bodies {
                        world.set_velocity(body, [unit() * 900.0, unit() * 900.0, -300.0 - (unit() + 1.0) * 350.0].into());
                    }
                    for _ in 0..240 {
                        world.step(1.0 / 60.0);
                        for &b in &bodies {
                            world.limit_speed(b, MAX_SPEED);
                        }
                    }
                    let axis = lowest_axis_end(&world, &bodies, &params, amplitude);
                    cases += 1;
                    worst = worst.min(axis);
                    limbs_under += usize::from(axis < -1.0);
                }
            }
        }
    }
    assert_eq!(limbs_under, 0, "{limbs_under} of {cases} corpses end with a limb under the ground, worst {worst}");
}

fn joint_gap(world: &World, bodies: &[BodyId], params: &RagdollParams) -> f32 {
    let at = |part: usize, anchor: Vector| {
        let (location, rotation) = world.pose(bodies[part]).unwrap();
        Frame::from_quat(rotation, 1.0, location.into()).transform_point(anchor.into())
    };
    params.joints.iter().map(|joint| {
        let (a, b) = (at(joint.child, joint.child_anchor), at(joint.parent, joint.parent_anchor));
        (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>().sqrt()
    }).fold(0.0, f32::max)
}

#[test]
fn real_skeleton_stays_on_the_ground() {
    let Some(rig) = rig() else { return };
    let mut failures = Vec::new();
    let mut worst_gap = 0.0f32;
    for (sequence, crouched) in [("c_beg1", true), ("c_beg2", true), ("c_base_beg", true), ("c_death", true), ("s_fall_fore", false)] {
        for fraction in [0.0, 0.25, 0.5, 0.75] {
            for amplitude in [0.0, 15.0, 40.0] {
                for shot in [0.0, 60.0, 300.0] {
                    let params = params(&rig, sequence, fraction, crouched).expect("a sequence of this skeleton");
                    let mut world = ground(amplitude);
                    let bodies = world.add_ragdoll(&params);
                    if shot > 0.0 {
                        let pelvis = bodies[1];
                        world.apply_impulse(pelvis, [shot, shot * 0.5, shot * 0.2].into(), None);
                    }
                    let height = |world: &World| bodies.iter().map(|&b| world.center_of_mass(b).unwrap()).map(|c| c[2] - ground_height(c[0], c[1], amplitude)).collect::<Vec<f32>>();
                    let start = height(&world);
                    let (mut top, mut rise, mut gap) = (0.0f32, 0.0f32, 0.0f32);
                    for _ in 0..420 {
                        world.step(1.0 / 60.0);
                        gap = gap.max(joint_gap(&world, &bodies, &params));
                        for &b in &bodies {
                            world.limit_speed(b, MAX_SPEED);
                            let v = world.velocity(b).unwrap();
                            top = top.max((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
                        }
                        rise = height(&world).iter().zip(&start).map(|(n, s)| n - s).fold(rise, f32::max);
                    }
                    let end = height(&world);
                    let axis = lowest_axis_end(&world, &bodies, &params, amplitude);
                    let under = end.iter().filter(|&&h| h < 0.0).count();
                    let lowest = end.iter().copied().fold(f32::MAX, f32::min);
                    let asleep = bodies.iter().all(|&b| world.is_sleeping(b));
                    let before: Vec<_> = bodies.iter().map(|&b| world.pose(b).unwrap()).collect();
                    world.wake_ragdoll(&bodies);
                    for _ in 0..30 {
                        world.step(1.0 / 60.0);
                    }
                    let turned = bodies.iter().zip(&before).map(|(&b, (_, q0))| {
                        let q1 = world.pose(b).unwrap().1;
                        2.0 * (0..4).map(|k| q0[k] * q1[k]).sum::<f32>().abs().min(1.0).acos().to_degrees()
                    }).fold(0.0f32, f32::max);
                    worst_gap = worst_gap.max(gap);
                    if under > 0 || lowest < 0.0 || axis < -1.0 || gap > 30.0 || (shot == 0.0 && top > 600.0) || (asleep && turned > 20.0) {
                        failures.push(format!("{sequence} {fraction} bumps {amplitude} shot {shot}: {under}/{} under, lowest {lowest:.1}, axis end {axis:.1}, top {top:.0}, rise {rise:.0}, gap {gap:.1}, asleep {asleep}, woken turn {turned:.1}", bodies.len()));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} cases, worst joint gap {worst_gap}:\n{}", failures.len(), failures.join("\n"));
}
