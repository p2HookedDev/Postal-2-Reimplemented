# Changelog

## 0.3.0

### New

- Water, fog and gravity areas work like the original game
- NPCs find more ways to reach you
- Hats, caps and glasses fly off when someone dies. You can shoot loose hats and purses or blow them around.
- Blood and bullet holes now show on doors, lifts and loose props, and move with them.
- Lifts and doors push and carry bodies and props.
- The icons in the main menu open their web pages.
- The Apocalypse Weekend intro shows its picture.
- Paradise Lost maps hide the things they are meant to hide in certain areas.

### Improved

- Much smoother play. The renderer no longer waits for the game logic, so the frame rate stays high in busy scenes.
- Everything moves evenly between game updates: people, ragdolls, held weapons, smoke, sparks and blood.
- Big fights cost less. Bodies that lie still, cut-off limbs and blood on the ground no longer slow the game down.
- Fewer short freezes when new things appear, during fights and when menu pictures load.
- Optimised multithreading
- People and objects are lit more like the original. A light behind a wall no longer lights them.
- See-through things such as smoke, glass and sky haze draw in the same order as the original.
- Looping animations play evenly, and an animation that starts again shows its first frame at once.
- Ragdoll knees and elbows bend the right way, and bodies grip the ground instead of sliding.

### Fixed

- NPCs move again after you load a quick save. They no longer float in a frozen pose.
- Police no longer repeat "drop your weapon" in a fast loop.
- The machine gun's muzzle flash no longer fills the screen.
- The black square around an NPC's muzzle flash is gone.
- Jump height is now corrected, when interacting with some level volumes.
- Ladders works
- The quick inventory works.
- The matinee no longer shakes or turns with the mouse.
- Some meshes werent lit correctly, now their materials are handled.
- The game no longer quits with an error at map start after you change a texture setting.
- The protest sign no longer shows a shovel inside it.
- The console no longer says "Unrecognized command" after a command that worked.
- Sky details that were missing are now rendered correctly.
- A weapon no longer flashes in a wrong pose for one frame when you draw it.
- Loose props slow down like the original instead of sliding too far.

### Other

- The engine was reorganised into smaller parts with clear jobs, and large files were split. You won't see this, but it makes future work faster and safer.
- More automated tests, including tests that run without the game installed.

## 0.2.0

### New

- Water reacts now. Ponds and pools ripple when you shoot them or walk through them, on maps that allow it.
- Settings in the Performance menu take effect right away. Fog distance, texture detail, filtering and detail textures no longer need a map reload.
- Characters blend smoothly into their next animation instead of snapping to it.

### Improved

- Better frame rates. The world, sprites and particles all cost less to draw, and new effects no longer load on the main thread.
- Sharper textures and less colour banding.
- Fog and the light on your weapon and on people now fade when you walk between areas, instead of changing all at once.
- Water looks calmer, runs smoothly at any frame rate and shows the right material for each map.
- Ragdolls were rebuilt. They stay out of walls, and piles of bodies settle down.

### Fixed

- Physics no longer jumps around on a long frame or at 60 frames per second.
- Light glows only show near their light and no longer appear from too far away.
- The hurt bars and skull show on every hit.
- Quick key taps, mouse clicks and wheel turns are no longer lost.
- Mouse buttons no longer stick when the game window loses focus.
- The cursor locks again after you close the map.
- A broken music file no longer crashes the game.
- Sounds with no fade-in are no longer silent, and sounds no longer pop when stopped during a fade-in.
- The world no longer goes blank for a moment while a material loads.
- Shadows and decals no longer go wrong after a map change.
- Particles draw in the right order.
- Surfaces that change their look during play now draw correctly.

### Other

- Code cleanup and more automated tests. You won't see these, but they make future fixes safer.

## 0.1.0

First public release.
