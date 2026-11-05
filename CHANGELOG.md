# Changelog

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
