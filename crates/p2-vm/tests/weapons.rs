mod common;

use common::{Dude, game_dir, load_suburbs, object_of};

use p2_package::Loader;
use p2_vm::{Encroachment, HostCommand, ObjectId, Value, Vm};

impl Dude<'_> {
    fn weapon(&self) -> Option<ObjectId> {
        object_of(&self.vm, self.pawn, "Weapon")
    }

    fn describe(&self, object: Option<ObjectId>) -> String {
        object.map_or("None".to_owned(), |o| {
            let channel = self.vm.anim_channel(o, 0).map(|(c, _)| format!("{} f{:.3} r{:.3} t{:.3}", self.vm.store.name(c.sequence), c.frame, c.rate, c.tween_rate));
            format!("{} ({}) {channel:?}", self.vm.store.name(self.vm.store.object(o).class_name), self.vm.store.name(self.vm.state_name(o)))
        })
    }
}

impl Dude<'_> {
    fn until_ready(&mut self, class: &str, limit: usize) -> usize {
        for frame in 0..limit {
            if let Some(weapon) = self.weapon()
                && self.vm.store.name(self.vm.store.object(weapon).class_name).eq_ignore_ascii_case(class)
                && self.vm.store.name(self.vm.state_name(weapon)) == "Idle"
            {
                return frame;
            }
            self.frames(1, &[]);
        }
        panic!("{class} not ready after {limit} frames: weapon {}", self.describe(self.weapon()));
    }

    fn ammo(&self, class: &str) -> i32 {
        let ammo = object_of(&self.vm, self.item(class), "AmmoType").expect("ammo");
        match self.vm.store.get(ammo, "AmmoAmount") {
            Some(Value::Int(amount)) => amount,
            other => panic!("AmmoAmount {other:?}"),
        }
    }

    fn look(&mut self, yaw: i32) {
        let rotation = Value::Rotator(p2_math::Rotator::new(0, yaw, 0));
        self.vm.store.set(self.controller, "Rotation", rotation.clone());
        self.vm.store.set(self.pawn, "Rotation", rotation);
    }

    fn bystander_in_front(&mut self, distance: f32) -> ObjectId {
        let me = self.vector(self.pawn, "Location");
        let candidates: Vec<ObjectId> = self
            .vm
            .actors()
            .filter(|&a| a != self.pawn && self.vm.store.is_a(a, "P2MoCapPawn") && !self.vm.store.is_deleted(a))
            .filter(|&a| object_of(&self.vm, a, "Controller").is_some() && self.float(a, "Health") > 0.0)
            .collect();
        let pawn = candidates.into_iter().min_by(|&a, &b| {
            let d = |p: ObjectId| {
                let at = self.vector(p, "Location");
                (0..3).map(|i| (at[i] - me[i]).powi(2)).sum::<f32>()
            };
            d(a).total_cmp(&d(b))
        });
        let pawn = pawn.expect("a bystander");
        let direction = |yaw: i32| {
            let angle = yaw as f32 * std::f32::consts::PI / 32768.0;
            [angle.cos(), angle.sin()]
        };
        let yaw = (0..16)
            .map(|i| i * 4096)
            .find(|&yaw| {
                let d = direction(yaw);
                let far = [me[0] + d[0] * distance * 2.0, me[1] + d[1] * distance * 2.0, me[2]];
                self.vm.trace_world(me.into(), far.into(), [0.0; 3].into()).is_none()
            })
            .expect("a free direction");
        self.look(yaw);
        let d = direction(yaw);
        let at = [me[0] + d[0] * distance, me[1] + d[1] * distance, me[2]];
        self.vm.place_far_moved(pawn, at.into(), Encroachment::Ignored).unwrap();
        let facing = Value::Rotator(p2_math::Rotator::new(0, yaw + 32768, 0));
        self.vm.store.set(pawn, "Rotation", facing);
        self.vm.mark_rendered(pawn).unwrap();
        pawn
    }
}

impl Dude<'_> {
    fn take_sounds(&mut self) -> Vec<String> {
        self.vm.take_commands(|command| matches!(command, HostCommand::Sound(_))).filter_map(|command| if let HostCommand::Sound(s) = command { Some(s.sound) } else { None }).collect()
    }

    fn sound_of(&self, object: ObjectId, property: &str) -> String {
        let sound = object_of(&self.vm, object, property).unwrap_or_else(|| panic!("no {property}"));
        self.vm.store.object(sound).path.clone()
    }

    fn fire(&mut self, key: &str, held: usize, total: usize) -> Vec<ObjectId> {
        let before: Vec<ObjectId> = self.vm.actors().collect();
        let mut spawned = Vec::new();
        for frame in 0..total {
            let keys: &[&str] = if frame < held { &[key] } else { &[] };
            self.frames(1, keys);
            for actor in self.vm.actors().filter(|a| !before.contains(a) && self.vm.store.is_a(*a, "Projectile")) {
                if !spawned.contains(&actor) {
                    spawned.push(actor);
                }
            }
        }
        spawned
    }
}

#[test]
fn selector_switch_completes_and_fires() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    let health = dude.float(dude.pawn, "Health");
    dude.frames(1, &["3"]);
    dude.frames(2, &[]);
    let shotgun = dude.item("ShotGunWeapon");
    let ammo = dude.ammo("ShotGunWeapon");
    let mut ready = None;
    for frame in 0..60 {
        let keys: &[&str] = if frame < 45 { &["LeftMouse"] } else { &[] };
        dude.frames(1, keys);
        if ready.is_none() && dude.weapon() == Some(shotgun) && dude.vm.store.name(dude.vm.state_name(shotgun)) != "Active" {
            ready = Some(frame);
        }
    }
    assert_eq!(dude.weapon(), Some(shotgun), "weapon {}", dude.describe(dude.weapon()));
    let ready = ready.expect("the shotgun never came up");
    assert!((10..40).contains(&ready), "brought up after {ready} frames");
    assert!(dude.ammo("ShotGunWeapon") < ammo, "never fired");
    assert_eq!(dude.float(dude.pawn, "Health"), health, "the dude hurt himself");
}

#[test]
fn pistol_comes_up_and_hits() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    dude.pick("PistolWeapon");
    let frames = dude.until_ready("PistolWeapon", 90);
    assert!((5..45).contains(&frames), "pistol ready after {frames} frames");
    let victim = dude.bystander_in_front(200.0);
    let (health, ammo) = (dude.float(victim, "Health"), dude.ammo("PistolWeapon"));
    dude.take_sounds();
    dude.fire("LeftMouse", 2, 20);
    let sounds = dude.take_sounds();
    let fire = dude.sound_of(dude.item("PistolWeapon"), "FireSound");
    assert_eq!(dude.ammo("PistolWeapon"), ammo - 1);
    assert!(sounds.contains(&fire), "{fire} not in {sounds:?}");
    assert!(dude.float(victim, "Health") < health, "the bystander was not hurt");
}

#[test]
fn shotgun_and_machinegun_hit() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    for class in ["ShotGunWeapon", "MachineGunWeapon"] {
        let mut dude = Dude::armed(&loader, &dir);
        dude.pick(class);
        dude.until_ready(class, 90);
        let victim = dude.bystander_in_front(200.0);
        let (health, ammo) = (dude.float(victim, "Health"), dude.ammo(class));
        dude.fire("LeftMouse", 15, 30);
        assert!(dude.ammo(class) < ammo, "{class} did not fire");
        assert!(dude.float(victim, "Health") < health, "{class} did not hurt the bystander");
    }
}

#[test]
fn shovel_and_kick_hit() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    let victim = dude.bystander_in_front(60.0);
    let health = dude.float(victim, "Health");
    dude.take_sounds();
    dude.fire("Q", 1, 20);
    let sounds = dude.take_sounds();
    assert!(dude.float(victim, "Health") < health, "the kick did not hurt");
    assert!(sounds.iter().any(|s| s.contains("foot_kick")), "{sounds:?}");

    let mut dude = Dude::armed(&loader, &dir);
    dude.pick("ShovelWeapon");
    dude.until_ready("ShovelWeapon", 90);
    let victim = dude.bystander_in_front(70.0);
    let health = dude.float(victim, "Health");
    dude.fire("LeftMouse", 2, 30);
    assert!(dude.float(victim, "Health") < health, "the shovel did not hurt");
}

#[test]
fn grenade_and_molotov_fly_and_explode() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    for (class, explosion) in [("GrenadeWeapon", "grenade_explode"), ("MolotovWeapon", "molotov_explode")] {
        let mut dude = Dude::armed(&loader, &dir);
        dude.pick(class);
        dude.until_ready(class, 120);
        let victim = dude.bystander_in_front(300.0);
        let (health, ammo) = (dude.float(victim, "Health"), dude.ammo(class));
        let start = dude.vector(dude.pawn, "Location");
        dude.take_sounds();
        let (mut projectile, mut far) = (None, 0.0f32);
        for frame in 0..300 {
            let thrown = dude.fire("LeftMouse", usize::from(frame < 3), 1);
            projectile = projectile.or(thrown.first().copied());
            let Some(projectile) = projectile else { continue };
            if dude.vm.store.is_deleted(projectile) {
                break;
            }
            let at = dude.vector(projectile, "Location");
            far = far.max(((at[0] - start[0]).powi(2) + (at[1] - start[1]).powi(2)).sqrt());
            if class == "GrenadeWeapon" && dude.vm.store.get(projectile, "Physics") == Some(Value::Byte(0)) {
                let height = dude.float(victim, "CollisionHeight");
                dude.vm.place_far_moved(victim, [at[0] + 50.0, at[1], at[2] + height].into(), Encroachment::Ignored).unwrap();
            }
        }
        let projectile = projectile.unwrap_or_else(|| panic!("{class}: nothing thrown"));
        dude.frames(30, &[]);
        let sounds = dude.take_sounds();
        assert_eq!(dude.ammo(class), ammo - 1, "{class}: one thrown");
        assert!(far > 100.0, "{class}: flew {far} units");
        assert!(dude.vm.store.is_deleted(projectile), "{class}: never exploded");
        assert!(sounds.iter().any(|s| s.contains(explosion)), "{class}: {sounds:?}");
        assert!(dude.float(victim, "Health") < health, "{class}: the blast did not hurt the bystander");
    }
}
#[test]
fn heads_end_their_channels_quietly() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    for _ in 0..60 {
        for pawn in dude.vm.actors().filter(|&a| dude.vm.store.is_a(a, "Pawn")).collect::<Vec<_>>() {
            dude.vm.mark_rendered(pawn).unwrap();
        }
        dude.frames(1, &[]);
    }
    let unexpected = dude.vm.diagnostics.log.iter().filter(|l| l.contains("Unexpected channel ended")).count();
    assert_eq!(unexpected, 0);
    assert!(dude.vm.diagnostics.missing_natives.is_empty(), "missing natives {:?}", dude.vm.diagnostics.missing_natives);
}

#[test]
fn bone_coords_follow_the_pose() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::armed(&loader, &dir);
    let victim = dude.bystander_in_front(200.0);
    let coords = |dude: &mut Dude, bone: &str| {
        let bone = dude.vm.intern(bone);
        let value = dude.vm.call_native("GetBoneCoords", Some(victim), &mut [Value::Name(bone)]).unwrap();
        let Some(Value::Struct(parts)) = value else { panic!("{value:?}") };
        let v = |p: &Value| match p {
            Value::Vector(v) => <[f32; 3]>::from(*v),
            _ => [0.0; 3],
        };
        (v(&parts[0]), v(&parts[1]))
    };
    let head_bone = match dude.vm.store.get(victim, "HeadBone") {
        Some(Value::Name(name)) => dude.vm.store.name(name).to_owned(),
        _ => "MALE01 head".to_owned(),
    };
    let (head, x_axis) = coords(&mut dude, &head_bone);
    let at = dude.vector(victim, "Location");
    let height = dude.float(victim, "CollisionHeight");
    assert!((head[0] - at[0]).hypot(head[1] - at[1]) < 30.0, "head off the pawn: {head:?} vs {at:?}");
    assert!(head[2] > at[2] + height * 0.5 && head[2] < at[2] + height + 20.0, "head at {head:?}, pawn {at:?} height {height}");
    let length = (x_axis[0].powi(2) + x_axis[1].powi(2) + x_axis[2].powi(2)).sqrt();
    assert!((length - 1.0).abs() < 1e-3);
    let (origin, axis) = coords(&mut dude, "no such bone");
    assert_eq!((origin, axis), ([0.0; 3], [1.0, 0.0, 0.0]));
}

#[test]
fn same_name_mesh_and_animation_stay_apart() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    loader.load("ED_Weapons").unwrap();
    let mut vm = Vm::new(&loader);
    let path = "ED_Weapons.ED_MP5_NEW";
    let animation = vm.stub(path, "MeshAnimation");
    let mesh = vm.stub(path, "SkeletalMesh");
    assert_ne!(animation, mesh);
    assert_eq!(vm.stub(path, "SkeletalMesh"), mesh);
    assert_eq!(vm.store.name(vm.store.object(mesh).class_name), "SkeletalMesh");
    let class = vm.stub("Engine.Mesh", "Class");
    let found = vm.call_native("DynamicLoadObject", None, &mut [Value::Str(path.to_owned()), Value::Object(Some(class)), Value::Bool(false)]).unwrap();
    assert_eq!(found, Some(Value::Object(Some(mesh))));
}
