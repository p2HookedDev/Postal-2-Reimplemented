mod common;

use common::{game_dir, ini, loader};

use p2_package::Loader;
use p2_vm::{HostCommand, SaveGame, Value, Vm};

fn live_actors(vm: &Vm<&Loader>) -> usize {
    let of_hud = |actor| vm.store.is_a(actor, "FontInfo") || vm.store.is_a(actor, "FPSButtonInfo");
    vm.actors().filter(|&actor| !vm.store.is_deleted(actor) && !of_hud(actor)).count()
}

#[test]
fn church_saves_and_loads() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("User", ini(&dir, "User.ini"));
    let game = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game, "").unwrap().is_empty());
    vm.init_client().unwrap();
    let (_, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.expect("pawn");
    for _ in 0..40 {
        vm.tick_level(0.05);
    }
    let answer = vm.call_native("ConsoleCommand", None, &mut [Value::Str("SaveGame 5".to_owned())]).unwrap();
    assert_eq!(answer, Some(Value::Str("1".to_owned())));
    let save = vm
        .take_commands(|command| matches!(command, HostCommand::SaveGame { .. }))
        .find_map(|command| if let HostCommand::SaveGame { slot: 5, save } = command { Some(save) } else { None })
        .expect("save queued");
    let bytes = save.encode().unwrap();
    let save = SaveGame::decode(&bytes).unwrap();
    assert_eq!(save.map, "Church");

    let mut loaded = Vm::new(&loader);
    loaded.add_config("User", ini(&dir, "User.ini"));
    loaded.restore(map, &save).unwrap();
    loaded.init_client().unwrap();
    let (_, loaded_pawn) = loaded.login("").unwrap().unwrap();
    let loaded_pawn = loaded_pawn.expect("saved pawn found by Login");
    let errors = loaded.post_load_game();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(loaded.store.object(loaded_pawn).path, vm.store.object(pawn).path);
    for property in ["Location", "Health"] {
        assert_eq!(loaded.store.get(loaded_pawn, property), vm.store.get(pawn, property), "{property}");
    }
    let (before, after) = (live_actors(&vm), live_actors(&loaded));
    assert!(before.abs_diff(after) <= 2, "saved {before}, loaded {after}");
    for _ in 0..20 {
        assert!(loaded.tick_level(0.05).is_empty());
    }
}
