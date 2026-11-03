//! The main thread runs systems too (Bevy's executor helps while it waits): it gets the same large stack as the
//! compute threads (`p2_engine::play::SCRIPT_STACK`), for the VM's script recursion.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:67108864");
    }
}