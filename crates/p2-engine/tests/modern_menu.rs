mod common;

use common::game_dir;

use p2_package::Loader;
use p2_vm::Vm;

#[test]
fn our_options_page_loads_and_replaces_the_retail_one() {
    let Some(dir) = game_dir() else { return };
    let Some(own) = dirs::config_dir().map(|dir| dir.join("p2-reimplemented").join("Packages")) else { return };
    if !own.join("P2Modern.u").is_file() {
        eprintln!("P2Modern.u not built, skipping");
        return;
    }
    let pattern = own.join("*.u").to_string_lossy().into_owned();
    let mut loader = Loader::open_with(&dir.join("System"), &[pattern]).unwrap();
    loader.load_scripts().unwrap();
    let mut vm = Vm::new(&loader);
    for class in ["ModernMenuOptions", "ModernMenuGraphics"] {
        assert!(vm.find_class("P2Modern", class).is_ok(), "P2Modern.{class} did not load");
    }
    assert!(vm.redirect_class(("Shell", "MenuOptions"), ("P2Modern", "ModernMenuOptions")), "the Options redirect did not take");
}
