mod common;

use common::{DT, Dude, game_dir, load_suburbs, object_of};

use p2_vm::{ObjectId, Value};

const PHYS_KARMA_RAGDOLL: u8 = 14;

impl Dude<'_> {
    fn physics(&self, actor: ObjectId) -> u8 {
        match self.vm.get(actor, "Physics") {
            Some(Value::Byte(b)) => b,
            other => panic!("Physics {other:?}"),
        }
    }

    fn ready_shotgun(&mut self) {
        let shotgun = self.item("ShotGunWeapon");
        self.pick("ShotGunWeapon");
        for _ in 0..90 {
            if object_of(&self.vm, self.pawn, "Weapon") == Some(shotgun) && self.vm.name(self.vm.state_name(shotgun)) == "Idle" {
                return;
            }
            self.frames(1, &[]);
        }
        panic!("the shotgun never came up");
    }

    fn bystander_in_front(&mut self, distance: f32) -> (ObjectId, [f32; 2]) {
        let me = self.vector(self.pawn, "Location");
        let pawn = self
            .vm
            .actors()
            .filter(|&a| a != self.pawn && self.vm.is_a(a, "P2MoCapPawn") && !self.vm.is_deleted(a))
            .filter(|&a| object_of(&self.vm, a, "Controller").is_some() && self.float(a, "Health") > 0.0)
            .min_by(|&a, &b| {
                let d = |p: ObjectId| {
                    let at = self.vector(p, "Location");
                    (0..3).map(|i| (at[i] - me[i]).powi(2)).sum::<f32>()
                };
                d(a).total_cmp(&d(b))
            })
            .expect("a bystander");
        let direction = |yaw: i32| {
            let angle = yaw as f32 * std::f32::consts::PI / 32768.0;
            [angle.cos(), angle.sin()]
        };
        let yaw = (0..16)
            .map(|i| i * 4096)
            .find(|&yaw| {
                let d = direction(yaw);
                let far = [me[0] + d[0] * distance * 3.0, me[1] + d[1] * distance * 3.0, me[2]];
                self.vm.trace_world(me.into(), far.into(), [0.0; 3].into()).is_none()
            })
            .expect("a free direction");
        let rotation = Value::Rotator(p2_math::Rotator::new(0, yaw, 0));
        self.vm.set(self.controller, "Rotation", rotation.clone());
        self.vm.set(self.pawn, "Rotation", rotation);
        let d = direction(yaw);
        self.vm.set_location(pawn, [me[0] + d[0] * distance, me[1] + d[1] * distance, me[2]].into()).unwrap();
        self.vm.set(pawn, "Rotation", Value::Rotator(p2_math::Rotator::new(0, yaw + 32768, 0)));
        self.vm.mark_rendered(pawn).unwrap();
        (pawn, d)
    }
}

const SETTLED_SPEED: f32 = 15.0;

#[test]
fn shot_bystander_ragdolls_falls_and_rests() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    dude.ready_shotgun();
    let (victim, direction) = dude.bystander_in_front(160.0);
    dude.frames(2, &[]);
    let start = dude.vector(victim, "Location");
    let floor = dude.vm.trace_world(start.into(), [start[0], start[1], start[2] - 500.0].into(), [0.0; 3].into()).expect("a floor").location[2];
    let mut died = None;
    for frame in 0..150 {
        let keys: &[&str] = if frame % 20 < 2 { &["LeftMouse"] } else { &[] };
        dude.frames(1, keys);
        if dude.physics(victim) == PHYS_KARMA_RAGDOLL {
            died = Some(frame);
            break;
        }
    }
    let karma_log: Vec<String> = dude.vm.diagnostics.log.iter().filter(|l| l.contains("Karma") || l.to_ascii_lowercase().contains("ragdoll")).cloned().collect();

    assert!(
        died.is_some(),
        "no ragdoll: health {}, physics {}, state {}, KParams {:?}; log {karma_log:?}",
        dude.float(victim, "Health"),
        dude.physics(victim),
        dude.vm.name(dude.vm.state_name(victim)),
        object_of(&dude.vm, victim, "KParams").map(|k| dude.vm.object(k).path.clone())
    );
    let params = object_of(&dude.vm, victim, "KParams").expect("KParams");
    let skeleton = dude.vm.get(params, "KSkeleton");
    assert!(matches!(skeleton, Some(Value::Name(_))), "KSkeleton {skeleton:?}");
    let bones = dude.vm.ragdoll_pose(victim).expect("a ragdoll pose").len();
    let died_at = dude.vector(victim, "Location");

    let (mut fastest, mut lowest, mut last_second) = (0.0_f32, f32::MAX, 0.0_f32);

    for frame in 0..300 {
        dude.frames(1, &[]);
        let velocity = dude.vector(victim, "Velocity");
        let speed = (0..3).map(|i| velocity[i] * velocity[i]).sum::<f32>().sqrt();
        fastest = fastest.max(speed);
        for bone in BONES {
            lowest = lowest.min(bone_origin(&mut dude, victim, bone)[2]);
        }
        if frame >= 270 {
            last_second = last_second.max(speed);
        }

        assert_eq!(dude.vm.ragdoll_pose(victim).map(|p| p.len()), Some(bones));
    }
    let end = dude.vector(victim, "Location");
    let head = bone_origin(&mut dude, victim, "MALE01 head");
    let pelvis = bone_origin(&mut dude, victim, "MALE01 pelvis");
    let pushed = (end[0] - died_at[0]) * direction[0] + (end[1] - died_at[1]) * direction[1];
    assert_eq!(dude.physics(victim), PHYS_KARMA_RAGDOLL);
    assert!(head[2] - floor < 30.0 && pelvis[2] - floor < 30.0, "not lying: head {head:?}, pelvis {pelvis:?}, floor {floor}");
    assert!(lowest > floor - 2.0, "a bone went below the floor: {lowest} < {floor}");
    assert!(fastest <= 1000.5, "fastest {fastest}");
    let moved = ((end[0] - died_at[0]).powi(2) + (end[1] - died_at[1]).powi(2)).sqrt();
    assert!(moved < 600.0, "the body flew {moved} units");
    assert!(pushed > 5.0, "not pushed away from the shooter: {pushed}");
    assert!(last_second < SETTLED_SPEED, "still moving after 10 s: {last_second} units / s");

    dude.vm.call_native("KFreezeRagdoll", Some(victim), &mut []).unwrap();
    dude.frames(10, &[]);
    assert_eq!(bone_origin(&mut dude, victim, "MALE01 head"), head, "the frozen head moved");
    assert_eq!(dude.vm.ragdoll_pose(victim).map(|p| p.len()), Some(bones));
}

#[test]
fn shot_ragdolls_come_to_rest() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    dude.ready_shotgun();
    let (victim, _) = dude.bystander_in_front(160.0);
    dude.frames(2, &[]);
    let mut last_second = 0.0_f32;
    for frame in 0..360 {
        let keys: &[&str] = if frame % 20 < 2 && dude.physics(victim) != PHYS_KARMA_RAGDOLL { &["LeftMouse"] } else { &[] };
        dude.frames(1, keys);
        if frame >= 330 {
            let velocity = dude.vector(victim, "Velocity");
            last_second = last_second.max((0..3).map(|i| velocity[i] * velocity[i]).sum::<f32>().sqrt());
        }
    }
    assert!(dude.vm.ragdoll_pose(victim).is_some(), "no ragdoll");
    assert!(last_second < SETTLED_SPEED, "still moving after 12 s: {last_second} units / s");
}

#[test]
#[ignore = "benchmark"]
fn monday_kills_frame_spikes() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    let started = std::time::Instant::now();
    dude.vm.prewarm();
    eprintln!("prewarm {:?}", started.elapsed());
    dude.ready_shotgun();
    let mut frames: Vec<(usize, std::time::Duration, String, Vec<(String, std::time::Duration)>)> = Vec::new();
    let frame = |frames: &mut Vec<_>, dude: &mut Dude, keys: &[&str], event: String| {
        for pawn in dude.vm.actors().filter(|&a| dude.vm.is_a(a, "Pawn") && !dude.vm.is_deleted(a)).collect::<Vec<_>>() {
            dude.vm.mark_rendered(pawn).unwrap();
        }
        dude.vm.diagnostics.tick_profile = Some(Default::default());
        let started = std::time::Instant::now();
        dude.vm.player_input(keys, [0.0, 0.0]).unwrap();
        let input = started.elapsed();
        dude.vm.tick_level(DT);
        let hud = std::time::Instant::now();
        dude.vm.render_hud(dude.controller, 1024.0, 768.0).unwrap();
        let (hud, time) = (hud.elapsed(), started.elapsed());
        let mut profile: Vec<(String, std::time::Duration)> = dude.vm.diagnostics.tick_profile.take().unwrap().into_iter().collect();
        profile.extend([("player input".to_owned(), input), ("render_hud".to_owned(), hud)]);
        profile.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        profile.truncate(8);
        frames.push((frames.len(), time, event, profile));
    };
    for kill in 0..10 {
        let (victim, _) = dude.bystander_in_front(160.0);
        frame(&mut frames, &mut dude, &[], format!("victim {kill} placed"));
        for shot in 0..150 {
            let keys: &[&str] = if shot % 20 < 2 { &["LeftMouse"] } else { &[] };
            frame(&mut frames, &mut dude, keys, String::new());
            if dude.physics(victim) == PHYS_KARMA_RAGDOLL {
                frames.last_mut().unwrap().2 = format!("victim {kill} ragdolls");
                break;
            }
        }
        for _ in 0..30 {
            frame(&mut frames, &mut dude, &[], String::new());
        }
    }
    let mut times: Vec<std::time::Duration> = frames.iter().map(|f| f.1).collect();
    times.sort();
    let median = times[times.len() / 2];
    eprintln!("{} frames, median {median:?}, 90 % {:?}, slowest {:?}", frames.len(), times[times.len() * 9 / 10], times.last().unwrap());
    let mut slow: Vec<_> = frames.iter().filter(|f| f.1 > median * 2).collect();
    slow.sort_by_key(|f| std::cmp::Reverse(f.1));
    for (index, time, event, profile) in slow.iter().take(24) {
        let parts: Vec<String> = profile.iter().map(|(n, t)| format!("{n} {t:?}")).collect();
        eprintln!("frame {index} {time:?} {event}: {}", parts.join(", "));
    }
    for (index, time, event, _) in frames.iter().filter(|f| !f.2.is_empty()) {
        eprintln!("frame {index} {time:?}: {event}");
    }
}

const BONES: [&str; 9] = [
    "MALE01 head",
    "MALE01 pelvis",
    "MALE01 spine2",
    "MALE01 L Calf",
    "MALE01 r calf",
    "MALE01 L Foot",
    "MALE01 r foot",
    "MALE01 L Hand",
    "MALE01 r hand",
];

fn bone_origin(dude: &mut Dude, actor: ObjectId, bone: &str) -> [f32; 3] {
    let name = dude.vm.intern(bone);
    let coords = dude.vm.call_native("GetBoneCoords", Some(actor), &mut [Value::Name(name)]).unwrap();
    match coords {
        Some(Value::Struct(fields)) => match &fields[0] {
            Value::Vector(v) => <[f32; 3]>::from(*v),
            other => panic!("{other:?}"),
        },
        other => panic!("GetBoneCoords {other:?}"),
    }
}
