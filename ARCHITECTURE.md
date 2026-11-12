# Architecture

P2Reimplemented is an open reimplementation of the POSTAL 2 engine (Unreal Engine 2 with Karma physics) in Rust and
the Bevy game engine. It runs the original game data from the player's own install, including the game's compiled
UnrealScript, which a virtual machine (the VM) executes.

This page is the codemap: what each crate is, what it may depend on, how one frame runs, and the rules that hold
everywhere.

## Invariants
- Retail, the shipped game, defines WHAT the game does: we learn it by reading the retail DLLs in the Ghidra
  disassembler. HOW is ours. A known difference from retail in a rule, event or visible result is a bug, unless we
  chose the difference on purpose.
- A crate depends only on crates in a lower layer, or on one before it in its own layer's chain (see "Layers"). Only
  tests may reach higher (`p2-particles`' tests use `p2-vm`). Nothing below layer 4 knows Bevy.
- The crates that do not know Bevy hold Unreal-space values as `p2_math::Vector` / `Rotator`; conversion
  to Bevy types happens only in the adapters (layer 4).
- The VM affects the host (the Bevy side, `p2-engine`) only through `HostCommand`; the host reads VM state,
  never writes it outside the VM's own API.
- The VM's actors are not copied into Bevy entities. Bevy's ECS holds only what is drawn and heard.
- One plugin per feature. A plugin registers its own systems, resources and messages. The order between plugins is
  set in one place, `FrameOrderPlugin`.
- Code that draws an actor gets its place only from the `Places` store (see "How a frame runs"). It cannot read the
  actor's stored location.
- Game data is loaded from the user's install at run time; no game files or decompiled code in the repo.
- No `P2_*` environment switches for behaviour; options are settings.

## Layers
```
L0 maths       p2-math
L1 formats     p2-package -> p2-script -> p2-assets
L2 algorithms  p2-collision -> p2-render-cpu -> p2-bake, p2-physics, p2-particles, p2-movie
L3 runtime     p2-vm
L4 adapters    p2-bevy-assets -> p2-engine
L5 binaries    p2-game, p2-viewer, p2-dump
test support   p2-test-package (a dev-dependency only)
```
An arrow is a chain inside a layer: the crate on the right may use the ones on its left.

## Crates
- `p2-math`: Unreal vector and rotator maths (`Vector`, `Rotator`) and small shared helpers.
- `p2-package`: Unreal package files: header, name / import / export tables, object property tags (memory-mapped).
- `p2-script`: UnrealScript definitions in `.u` packages: classes, structs, states, functions, bytecode decoding.
- `p2-assets`: native data of `Engine` classes (meshes, textures, BSP, terrain, sounds), kept in stored format.
- `p2-collision`: level BSP queries (point, line, extent) and triangle sweeps; `tree` is a bounding-box tree over a
  placed mesh's triangles, built at load, that narrows box queries.
- `p2-render-cpu`: CPU side of rendering: static-mesh lighting, zone portal visibility, procedural textures.
- `p2-bake`: relights a map's BSP light maps from its own lights (soft shadows, one bounce, occlusion) and caches
  them; used while the modern "Relit world" option is on.
- `p2-physics`: rigid bodies and ragdolls for Karma actors, on the Rapier crate. It steps in fixed steps of 1/60 s
  and gives poses between steps.
- `p2-particles`: `Emitter` particle simulation as retail's `Engine.dll` runs it.
- `p2-movie`: RoQ movie decoder (video frames and sound), no engine dependency.
- `p2-vm`: the UnrealScript runtime (interpreter, closure compiler, object memory, natives) and the actor world it
  drives (actor ticks, pawn physics, volumes, AI, animation, Karma glue, HUD canvas, travel, save games). One `Vm`
  type today; see "Known debt".
- `p2-bevy-assets`: p2-assets types into Bevy assets (meshes, images).
- `p2-engine`: the Bevy host: package library, map scenes, materials and shaders, lights, cameras, the VM thread and
  play loop, sound, finding the install, save files. A binary adds it as one plugin group, `P2EnginePlugins`.
- `p2-game`: the game binary (launcher, input, Vulkan only). `p2-viewer`: the asset browser. `p2-dump`: package
  table dumper.
- `p2-test-package`: writes small real `.u` packages from declarations in test code, so tests run the normal load
  path without the game install.

## How a frame runs
- The VM runs on its own thread. The main thread draws. They meet once a frame at the hand-off
  (`PlaySystems::Tick`): the main thread takes the finished tick's `VmFrame` and gives the VM the next input.
- A frame never waits for a slow game tick. If the tick is not ready, the frame is drawn without it.
- Game ticks start at most 120 times a second. Frames are not capped.
- Between ticks the drawn world moves smoothly: actors, the camera, bones and ragdolls glide from the last tick's
  place to the newest one. The view also turns with mouse movement the game has not processed yet.
- One struct, `Shown`, holds everything a frame shows and has the only clock: the view, actor places (`Places`),
  animation times and ragdoll bones.
- Physics steps inside the game tick on the VM thread, so scripts see the same tick's results. There is no physics
  thread.
- Each object keeps a stamp of its last write. After a tick the host reads again only the actors that were written,
  so idle actors cost almost nothing.
- HUD scripts run at most about 60 times a second, unless a menu or the mouse cursor is shown.
- The game tick on the main thread is three systems: gather the input, hand off to the game, place the view. The
  present step after it is seven ordered systems (actors, particles, projectors, material switches, HUD and others).

## p2-vm modules
- Script runtime: `interp` (+ `interp/compile`, `call`, `eval`, `exec`, `lookup`, `place`, `run`), `memory`,
  `states`, `natives`, `native_id`, `util_natives`, `operators` (the operators and maths functions of `Core`), `enums`
  (engine enums that scripts keep in byte properties), `config`, `side_packages`, `class_load`, `script_hooks` (the
  one table of script functions the engine catches).
- Objects: `object` (an object and its memory; every write leaves a stamp), `object_store`, `object_table` (tables
  indexed by object id), `objects` (finding classes, making objects).
- Actor world: `actors` (+ `tick`, `touch`, `encroach`, `lifecycle`, `level`), `world`, `hash`, `collision`,
  `find_spot`, `volumes`, `deco_list` (decorations a volume spawns), `travel`, `props`, `save` (save games).
- Physics: `physics` (+ `walking`, `falling`, `fly_swim`, `ladder_spider`, `root_motion`, `sweep` and others),
  `projectile`, `mover`, `karma`, `ragdoll`, `limbs`, `bolton` (hats and caps that come off), `bullet_hole`.
- AI: `ai_natives`, `ai_move`, `movement_natives` (movement natives of `Controller` and `PlayerController`), `nav`,
  `nav_jump` (the jump reach test), `senses`.
- Media and presentation: `anim`, `pawn_anim`, `bones`, `matinee`, `canvas`, `projector`, `actor_natives`,
  `game_natives`, `damage`.
- Host side: `client`, `input`, `host_api` (what the host may read); diagnostics: `profiling`.

## p2-engine areas
- Plugins: `P2EnginePlugins` adds, in order, `FrameOrderPlugin`, `RetailRenderPlugin`, `MeshViewPlugin`,
  `DisplayPlugin`, `GammaPlugin`, `ModernGraphicsPlugin`, `AnimationPlugin`, `PlayPlugin`, `MapPlugin`. The viewer
  turns off the display, gamma and modern graphics plugins.
- Assets and settings: `library`, `loading`, `actors`, `display`, `game_dir`, `diagnostics`.
- Install and saves: `install` (finds and checks the player's install), `game_dir_form` (the folder form the launcher
  and the viewer share), `saves` (save files on disk).
- Shared data: `components` (render layers, follow / canvas / view components), `shown_scene`, `level_collision`.
- Scene: `map/` (`load` = map loads as a plugin, `scene` = scene build, spawned models, particles, sprites, coronas,
  fluids, movies, HUD), `emitter_changes`. Each of these keeps its state in its own resource. Entities made for a map
  carry a marker and are despawned by it when the map is left.
- Rendering, `render/`: materials and their shaders (`lightmapped`, `mesh_light`, `blended`, `actor_light`,
  `terrain_material`, `particle_material`, `shader_layers` with the `layered_extension!` macro, `shaders` with the
  `ShaderLibraryPlugin`), lights (`bsp_light` = the frame's dynamic lights, `terrain_light`), `world_pass` (the
  static world drawn from one table and culled on the GPU), `translucent_order` (retail's back-to-front list for
  see-through draws), `sampler_refresh` (texture settings act at once), the sun mask (`sun_mask`), `gamma`, `wet`,
  `modern` (optional modern graphics).
- View: `mesh_view` (cameras, sky, fog, zone culling, scene swap), `animation` (poses and things attached to bones).
- Play: `play/` (`vm_thread` = the VM thread and what the host reads from it, `world` = hand-off, tick clock,
  `Shown` and `Places`, `sound` = its own plugin, music).

## Known debt
- `Vm` groups its state in sub-structs (`store`: names, objects, classes; `script`: memory, functions, lookups,
  states; `media`; `host`; `diagnostics`; `options`) but its methods still live on `Vm` across most modules: moving
  single-subsystem methods onto their struct is still to do.
- Inside p2-engine the modules are meant to form layers, lowest first: `render`, `play`, `mesh_view`, `map`. Three
  imports still point upward into `mesh_view`: `render::actor_light` (`set_specular_uv`), `render::world_pass`
  (`SectionMaterial`) and `play` (`ShadowOnly`).
- The move to one plugin per feature is not finished: its last stage is still to do.

