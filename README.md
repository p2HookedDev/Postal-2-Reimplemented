# P2Reimplemented

<p align="center">
  <img src="https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/223470/logo.png" alt="POSTAL 2 logo" width="420">
</p>

<p align="center">
  An open reimplementation of the POSTAL 2 engine in Rust and Bevy.<br>
  It runs the original game from your own install, including its UnrealScript, on a new engine.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-in%20development-orange" alt="Status: in development">
  <img src="https://img.shields.io/badge/rust-1.99.1-b7410e?logo=rust" alt="Rust 1.99.1">
  <img src="https://img.shields.io/badge/bevy-0.20-232326" alt="Bevy 0.20">
  <img src="https://img.shields.io/badge/platform-Windows-0078d6" alt="Platform: Windows">
</p>

> **No game files are included.** You need your own copy of POSTAL 2 (Steam or GOG).
>
> The game starts and plays, but it is not finished. Expect bugs.

## Motivation

POSTAL 2 came out in 2003 on Unreal Engine 2 with Karma physics. It deserves to keep running well on modern machines.

This project keeps the game and replaces the engine under it: a new script VM, a new renderer and new physics. The aim
is the same game on a modern engine, not a copy of a 2003 one.

- **The shipped game defines what happens.** Rules, events and visible results should match it. A known difference is a bug.
- **How it happens is ours.** Internals are free to be faster and simpler, as long as scripts and players notice no loss.
- **Performance is a feature.** The engine should beat the original, then leave room for modern rendering.

## Features

The list below is implemented. Some of it still awaits a full check in the game.

**Game data**

- Reads packages, maps, textures, meshes, sounds and animations directly from your install
- Launcher that finds your install, or lets you pick the folder

**UnrealScript**

- VM that runs the game's compiled scripts, with an interpreter and a closure compiler
- Engine natives, script states, latent actions and config files

**World and gameplay**

- Actor world: pawn physics, projectiles, movers, volumes, level travel
- AI: path finding, movement, sight and hearing
- Skeletal animation with blending, facial animation, Matinee scenes and limb and head damage
- Save games, stored outside the install

**Physics**

- Karma actors and ragdolls, rebuilt on [Rapier](https://rapier.rs)

**Rendering and sound**

- BSP with lightmaps, static and skeletal meshes, terrain, zone portal visibility
- Particles, coronas, fluids, projectors, HUD canvas
- RoQ movies, positional sound and music
- Optional Modern Graphics menu, from our own script package `script/P2Modern`

**Tools**

- `p2-viewer` browses the install's packages and previews their assets
- `p2-dump` prints package tables

## Build and run

You need:

- Windows (developed on Windows 10)
- [rustup](https://rustup.rs); `rust-toolchain.toml` selects the Rust version
- Visual Studio Build Tools with the C++ workload
- POSTAL 2 installed
- About 10 GB of free disk space for build output

Then:

```
git clone https://github.com/p2HookedDev/Postal-2-Reimplemented.git
cd Postal-2-Reimplemented
cargo run -p p2-game
```

The first build takes a while. A launcher opens and finds your install, or asks for the folder.

An optional argument names the start map:

```
cargo run -p p2-game -- Church
```

Other programs:

```
cargo run -p p2-viewer
cargo run -p p2-dump -- <package file>
```

In the game, Esc releases the mouse and F10 opens the diagnostics overlay.

### Tests

```
cargo clippy --workspace --all-targets
cargo test --workspace
```

Tests that need game data read the install path from the environment variable `P2_GAME_DIR`, and skip when it is unset.

## Progress

| Milestone       | State                                                     |
| --------------- | --------------------------------------------------------- |
| Package reader  | Done                                                      |
| Asset viewer    | Done                                                      |
| Map rendering   | Nearly done: dynamic lights remain                        |
| UnrealScript VM | Runs the game; natives still being filled in              |
| Playable        | In progress: the game plays, polish and checks remain     |
| Full game       | In progress: physics, AI and saves work, mods not started |

## To do

- [ ] Play through the whole game and fix what it finds
- [ ] Finish dynamic and animated lights
- [ ] Draw warp zone portals
- [ ] Zone and volume ambient sounds
- [ ] Zone fog on the play camera, player shadows, dynamic weapon lights, widescreen HUD stretch
- [ ] Apply texture detail and filter changes without a map change
- [ ] Reduce hitches from spawns and piles of ragdolls
- [ ] Mod compatibility

## Plans

1. **Release readiness:** match the shipped game and close the gaps above.
2. **Faster core:** a flat script execution form in the VM, cheaper casts and calls, and our own GPU skinning.
3. **Modern rendering:** new rendering techniques on top of the same game, once the game itself is right.

## Known issues

- Warp zone portals are not drawn yet.
- Texture detail and filter changes apply from the next map.
- Zone ambient sounds are missing, and so are the rendering features listed under To do.
- Frame rate drops when many ragdolls pile up, and many spawns at once can cause a brief hitch.
- Particles may flicker or cut off at some camera angles.
- Corpses and physics props after loading a save are not fully checked.
- Mods are not supported yet.
- Windows only.

## AI disclosure

AI tools are used throughout this project. They do the reverse engineering of the shipped game: reading its DLLs and
working out what each system does. Large portion was written by hand, but using AI assistance, helping on finding bugs, cleaning up code.

## Rules for changes

Never commit game files (packages, maps, textures, sounds, binaries) or decompiled code. Assets load from the install at
run time. Notes on the original engine describe behaviour and layouts in our own words.

## License

[MIT](LICENSE). It covers this source code only, not POSTAL 2 or its data.

POSTAL 2 and its logo are trademarks of their owners. The logo above is loaded from Steam and is not part of this
repository. This project is not affiliated with or endorsed by them.
