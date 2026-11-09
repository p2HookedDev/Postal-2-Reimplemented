#![allow(dead_code, reason = "each test binary uses a part of this module")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use p2_package::{Loader, PackageId};
use p2_test_package::PackageBuilder;
use p2_vm::{ObjectId, Value, Vm};

pub fn load_fixture(package: &PackageBuilder, dir: &str) -> Vm<Arc<Loader>> {
    let dir = package.write_to_temp(dir).unwrap();
    let mut loader = Loader::new(&dir, &["*.u".into()]).unwrap();
    loader.load_scripts().unwrap();
    Vm::new(Arc::new(loader))
}

pub fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

pub fn ini(dir: &Path, name: &str) -> p2_package::Ini {
    p2_package::Ini::read(dir.join("System").join(name)).unwrap()
}

pub fn system_ini(dir: &Path) -> p2_package::Ini {
    let mut system = ini(dir, "Postal2.ini");
    system.set("Postal2Game.P2GameInfoSingle", "bAllowCManager", "True");
    system.set("Postal2Game.P2GameInfoSingle", "bWarnedCheater", "True");
    system
}

pub fn start_single_player<'a>(dir: &Path, loader: &'a Loader, id: PackageId) -> (Vm<&'a Loader>, ObjectId, Option<ObjectId>) {
    let mut vm = Vm::new(loader);
    vm.add_config("System", system_ini(dir));
    vm.add_config("User", ini(dir, "User.ini"));
    let game = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(id, game, "").unwrap();
    vm.init_client().unwrap();
    let (controller, pawn) = vm.login("").unwrap().unwrap();
    vm.options.player_physics = true;
    (vm, controller, pawn)
}

pub fn loader() -> Option<Loader> {
    let dir = game_dir()?;
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    Some(loader)
}

pub fn load(dir: &Path, map: &str) -> (Loader, PackageId) {
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let id = loader.load(map).unwrap();
    loader.load_imports(id).unwrap();
    (loader, id)
}

pub fn load_suburbs(dir: &Path) -> Loader {
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let id = loader.load("Suburbs-3").unwrap();
    loader.load_imports(id).unwrap();
    loader.load_imported(&["MeshAnimation", "SkeletalMesh"]).unwrap();
    loader
}

pub fn vector(vm: &Vm<&Loader>, object: ObjectId, property: &str) -> [f32; 3] {
    match vm.store.get(object, property) {
        Some(Value::Vector(v)) => <[f32; 3]>::from(v),
        _ => [0.0; 3],
    }
}

pub fn object_of(vm: &Vm<&Loader>, object: ObjectId, property: &str) -> Option<ObjectId> {
    match vm.store.get(object, property) {
        Some(Value::Object(found)) => found,
        _ => None,
    }
}

pub fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    a.map(|c| c * s)
}

pub const DT: f32 = 1.0 / 30.0;

pub struct Dude<'a> {
    pub vm: Vm<&'a Loader>,
    pub controller: ObjectId,
    pub pawn: ObjectId,
}

impl<'a> Dude<'a> {
    pub fn playing(loader: &'a Loader, dir: &Path) -> Self {
        let id = loader.loaded_id("Suburbs-3").unwrap();
        let (vm, controller, pawn) = start_single_player(dir, loader, id);
        Self { vm, controller, pawn: pawn.unwrap() }
    }

    pub fn armed(loader: &'a Loader, dir: &Path) -> Self {
        let mut dude = Self::playing(loader, dir);
        dude.frames(30, &[]);
        dude.console("PackNHeat");
        dude.frames(5, &[]);
        dude
    }

    pub fn console(&mut self, command: &str) {
        self.vm.call_native("ConsoleCommand", Some(self.controller), &mut [Value::Str(command.to_owned()), Value::Bool(false)]).unwrap();
    }

    pub fn frames(&mut self, count: usize, keys: &[&str]) {
        for _ in 0..count {
            self.vm.player_input(keys, [0.0, 0.0]).unwrap();
            let errors = self.vm.tick_level(DT);
            assert!(errors.is_empty(), "{:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
            self.vm.render_hud(self.controller, 1024.0, 768.0).unwrap();
        }
    }

    pub fn float(&self, object: ObjectId, property: &str) -> f32 {
        match self.vm.store.get(object, property) {
            Some(Value::Float(v)) => v,
            Some(Value::Int(v)) => v as f32,
            other => panic!("{property}: {other:?}"),
        }
    }

    pub fn vector(&self, object: ObjectId, property: &str) -> [f32; 3] {
        match self.vm.store.get(object, property) {
            Some(Value::Vector(v)) => <[f32; 3]>::from(v),
            other => panic!("{property}: {other:?}"),
        }
    }

    pub fn item(&self, class: &str) -> ObjectId {
        let mut next = object_of(&self.vm, self.pawn, "Inventory");
        for _ in 0..500 {
            let Some(item) = next else { break };
            if self.vm.store.name(self.vm.store.object(item).class_name).eq_ignore_ascii_case(class) {
                return item;
            }
            next = object_of(&self.vm, item, "Inventory");
        }
        panic!("no {class} in the inventory");
    }

    pub fn pick(&mut self, class: &str) {
        let weapon = self.item(class);
        self.vm.store.set(self.pawn, "PendingWeapon", Value::Object(Some(weapon)));
        let current = object_of(&self.vm, self.pawn, "Weapon").expect("a weapon in hand");
        let put_down = self.vm.find_object_function(current, "PutDown").unwrap().unwrap();
        let class = self.vm.store.object(current).class.unwrap();
        self.vm.call(&put_down, Some(current), class, Vec::new()).unwrap();
    }
}
