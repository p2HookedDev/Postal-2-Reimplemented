mod common;

use common::{Dude, game_dir, load_suburbs};
use std::path::Path;

use p2_package::Loader;
use p2_vm::{ObjectId, Value};

impl<'a> Dude<'a> {
    fn start(loader: &'a Loader, dir: &Path) -> Self {
        let mut dude = Self::playing(loader, dir);
        for _ in 0..300 {
            dude.frames(1, &[]);
            if dude.vm.store.name(dude.vm.state_name(dude.controller)) != "PlayerPrepSave" {
                break;
            }
        }
        dude.frames(5, &[]);
        dude
    }

    fn weapon(&self) -> (String, String) {
        match self.vm.store.get(self.pawn, "Weapon") {
            Some(Value::Object(Some(w))) => (self.vm.store.name(self.vm.store.object(w).class_name).to_owned(), self.vm.store.name(self.vm.state_name(w)).to_owned()),
            _ => (String::new(), String::new()),
        }
    }

    fn until_ready(&mut self, class: &str) {
        for _ in 0..150 {
            let (weapon, state) = self.weapon();
            if weapon == class && state == "Idle" {
                return;
            }
            self.frames(1, &[]);
        }
        panic!("{class} not ready: {:?}", self.weapon());
    }

    fn live(&self, class: &str) -> Vec<ObjectId> {
        self.vm.actors().filter(|&a| self.vm.store.is_a(a, class) && !self.vm.store.is_deleted(a)).collect()
    }

    fn flag(&self, object: ObjectId, property: &str) -> bool {
        self.vm.store.get(object, property) == Some(Value::Bool(true))
    }

    fn look_down(&mut self, pitch: i32) {
        let rotation = Value::Rotator(p2_math::Rotator::new(65536 - pitch, 0, 0));
        self.vm.store.set(self.controller, "Rotation", rotation);
    }
}

#[test]
fn urine_streams_to_the_floor_and_makes_a_growing_puddle() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::start(&loader, &dir);
    dude.frames(1, &["R"]);
    dude.until_ready("UrethraWeapon");
    dude.look_down(3000);
    let mut radii = Vec::new();
    for frame in 0..120 {
        dude.frames(1, &["LeftMouse"]);
        if frame % 30 == 29 {
            radii.push(dude.live("UrinePuddle").first().map(|&p| dude.float(p, "UseColRadius")));
        }
    }
    let feeder = *dude.live("UrinePourFeeder").first().expect("no urine stream");
    let (at, hit) = (dude.vector(dude.pawn, "Location"), dude.vector(feeder, "LastHitLocation"));
    let feet = at[2] - dude.float(dude.pawn, "CollisionHeight");
    let reach = (hit[0] - at[0]).hypot(hit[1] - at[1]);
    assert!((hit[2] - feet).abs() < 20.0 && (20.0..600.0).contains(&reach), "the stream lands at {hit:?}, the dude stands at {at:?}");
    let splash = *dude.live("UrineSplashEmitter").first().expect("no splash");
    let splash_emitter = match dude.vm.store.get(splash, "Emitters") {
        Some(Value::Array(e)) => match e[0] {
            Value::Object(Some(e)) => e,
            _ => panic!("no splash emitter"),
        },
        other => panic!("{other:?}"),
    };
    assert!(!dude.flag(splash_emitter, "Disabled"), "the splash is off while the stream hits the floor");
    let radii: Vec<f32> = radii.into_iter().flatten().collect();
    assert!(radii.len() >= 2 && radii.last() > radii.first(), "the puddle did not grow: {radii:?}");
    assert!(!dude.live("UrineTrail").is_empty(), "no trail on the floor");
    dude.frames(60, &[]);
    assert!(dude.live("UrinePourFeeder").is_empty(), "the stream goes on: {:?}", dude.weapon());
    assert!(dude.vm.diagnostics.missing_natives.is_empty(), "missing natives {:?}", dude.vm.diagnostics.missing_natives);
}

/// `RadiusActors(class'Fluid', CollisionRadius)` finds the puddle only because the engine adds each actor's own collision
/// radius.
#[test]
fn gasoline_pours_and_a_match_lights_it() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::start(&loader, &dir);
    dude.console("PackNHeat");
    dude.frames(5, &[]);
    dude.pick("GasCanWeapon");
    dude.until_ready("GasCanWeapon");
    dude.look_down(6000);
    dude.frames(120, &["LeftMouse"]);
    dude.frames(30, &[]);
    let puddle = *dude.live("GasPuddle").first().expect("no gas puddle");
    assert!(!dude.live("GasTrail").is_empty(), "no gas trail");
    assert!(!dude.flag(puddle, "bOnFire"));
    dude.frames(2, &["RightMouse"]);
    let mut lit = None;
    for frame in 0..150 {
        dude.frames(1, &[]);
        if lit.is_none() && dude.flag(puddle, "bOnFire") {
            lit = Some(frame);
        }
    }
    assert!(lit.is_some(), "the match did not light the gas");
    assert!(!dude.live("FirePuddle").is_empty() || dude.vm.actors().any(|a| dude.vm.store.is_a(a, "FirePuddle")), "no fire puddle");
    assert!(dude.vm.diagnostics.missing_natives.is_empty(), "missing natives {:?}", dude.vm.diagnostics.missing_natives);
}
