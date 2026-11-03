# Architecture

P2Reimplemented is an open reimplementation of the POSTAL 2 engine (Unreal Engine 2 with Karma physics) in Rust and
the Bevy game engine. It runs the original game data from the player's own install, including the game's compiled
UnrealScript, which a virtual machine (the VM) executes.

This page is the codemap: what each crate is, what it may depend on, and the rules that hold everywhere.

## Invariants
- Retail, the shipped game, defines WHAT the game does: we learn it by reading the retail DLLs in the Ghidra
  disassembler. HOW is ours. A known difference from retail in a rule, event or visible result is a bug.
- A crate depends only on crates in a lower layer, or on one before it in its own layer's chain (see "Layers"). Only
  tests may reach higher (`p2-particles`' tests use `p2-vm`). Nothing below layer 4 knows Bevy.
- The crates that do not know Bevy hold Unreal-space values as `p2_math::Vector` / `Rotator`; conversion
  to Bevy types happens only in the adapters (layer 4).
- The VM affects the host (the Bevy side, `p2-engine`) only through `HostCommand`; the host reads VM state,
  never writes it outside the VM's own API.
- Game data is loaded from the user's install at run time; no game files or decompiled code in the repo.
- No `P2_*` environment switches for behaviour; options are settings.

## Layers
```
L0 maths       p2-math
L1 formats     p2-package -> p2-script -> p2-assets
L2 algorithms  p2-collision, p2-physics, p2-particles, p2-render-cpu, p2-movie
L3 runtime     p2-vm
L4 adapters    p2-bevy-assets -> p2-engine
L5 binaries    p2-game, p2-viewer, p2-dump
```
An arrow is a chain inside a layer: the crate on the right may use the one on its left.

## Crates
- `p2-math`: Unreal vector and rotator maths (`Vector`, `Rotator`).
- `p2-package`: Unreal package files: header, name / import / export tables, object property tags (memory-mapped).
- `p2-script`: UnrealScript definitions in `.u` packages: classes, structs, states, functions, bytecode decoding.
- `p2-assets`: native data of `Engine` classes (meshes, textures, BSP, terrain, sounds), kept in stored format.
- `p2-collision`: level BSP queries (point, line, extent) and triangle sweeps; `tree` is a bounding-box tree over a
  placed mesh's triangles, built at load, that narrows box queries.
- `p2-physics`: rigid bodies and ragdolls for Karma actors, on the Rapier crate.
- `p2-particles`: `Emitter` particle simulation as retail's `Engine.dll` runs it.
- `p2-render-cpu`: CPU side of rendering: static-mesh lighting, zone portal visibility, procedural textures.
- `p2-movie`: RoQ movie decoder (video frames and sound), no engine dependency.
- `p2-vm`: the UnrealScript runtime (interpreter, closure compiler, object memory, natives) and the actor world it
  drives (actor ticks, pawn physics, AI, animation, Karma glue, HUD canvas, travel, save games). One `Vm` type today;
  see "Known debt".
- `p2-bevy-assets`: p2-assets types into Bevy assets (meshes, images).
- `p2-engine`: the Bevy host: package library, map scenes, materials and shaders, lights, cameras, the VM thread and
  play loop, sound, finding the install, save files.
- `p2-game`: the game binary. `p2-viewer`: the asset browser. `p2-dump`: package table dumper.

## p2-vm modules
- Script runtime: `interp` (+ `interp/compile`), `memory`, `states`, `natives`, `native_id`, `util_natives`,
  `operators` (the operators and maths functions of `Core`), `enums` (engine enums that scripts keep in byte
  properties, read at the property boundary), `config`, `side_packages`.
- Actor world: `actors`, `world`, `hash`, `collision`, `find_spot`, `volumes`, `travel`, `props`, `save` (save games).
- Physics: `physics` (+ `physics/fly_swim`, `ladder_spider`, `root_motion`), `projectile`, `mover`, `karma`,
  `ragdoll`, `limbs`.
- AI: `ai_natives`, `ai_move`, `movement_natives` (movement natives of `Controller` and `PlayerController`), `nav`,
  `senses`.
- Media and presentation: `anim`, `pawn_anim`, `bones`, `matinee`, `canvas`, `projector`, `actor_natives`,
  `game_natives`, `damage`.
- Host side: `client`, `input`; diagnostics: `profiling`, `parallel` (read-only work on threads).

## p2-engine areas
- Assets and settings: `library`, `loading`, `actors`, `display`, `game_dir`, `diagnostics`.
- Install and saves: `install` (finds and checks the player's install), `game_dir_form` (the folder form the launcher
  and the viewer share), `saves` (save files on disk).
- Shared data: `components` (render layers, follow / canvas / view components), `shown_scene`, `level_collision`.
- Scene: `map/` (scene build, session, spawned models, particles, sprites, coronas, fluids, movies, HUD),
  `emitter_changes`.
- Rendering, `render/`: materials and their shaders (`lightmapped`, `mesh_light`, `blended`, `actor_light`,
  `terrain_material`, `particle_material`, `shader_layers` with the `layered_extension!` macro, `shaders` with the `ShaderLibraryPlugin`),
  lights (`bsp_light` = the frame's dynamic lights, `terrain_light`), the sun mask (`sun_mask`), `gamma`, `wet`,
  `modern` (optional modern graphics).
- View: `mesh_view` (cameras, sky, zone visibility, scene swap), `animation`.
- Play: `play/` (VM thread, tick, sound, music).

## Known debt
- `Vm` groups its state in sub-structs (`store`: names, objects, classes; `script`: memory, functions, lookups,
  states; `media`; `host`; `diagnostics`; `options`) but its methods still live on `Vm` across most modules: moving
  single-subsystem methods onto their struct is still to do.
- Inside p2-engine the modules form layers too, lowest first: `render`, `play`, `mesh_view`, `map`; none imports one
  above it (`play` names `map` in a doc link only). `render` imports only `components`, `shown_scene`,
  `level_collision`, `display` and `library` (the VM frame through `render::actor_light::ActorFrames` and
  `FrameLights`).

