# P2Modern: the Modern Graphics menu package

Our own UnrealScript package that adds a "Modern Graphics" row to the Options menu and three pages for the modern
graphics: a main page (master switch, quality preset) and seven pages ("Shadows and lighting", "Image quality", "Post
processing", "Surfaces and reflections", "Blood and particles", "Screen and HUD", "Animation"), with combos,
sliders and boxes. It is our code, not retail's; it uses only the stock Shell menu helpers (`AddComboBox`,
`AddSlider`, `AddCheckbox`), so it looks like the retail pages.

## Classes

- `ModernMenuOptions` extends retail `Shell.MenuOptions`: the stock page, its last row (Back) relabelled
  "Modern Graphics", a fresh Back below it. The host swaps it in for `Shell.MenuOptions`.
- `ModernMenuMain` extends retail `Shell.MenuMain`: adds the POSTAL 4 banner, or the Paradise Lost one when POSTAL 4
  is owned, without retail's Steam build check. The host answers `SteamOwnsGame` / `SteamOwnsDLC` from Steam's
  installed apps.
- `ModernMenuPage` extends `Shell.ShellMenuCW`: the shared base. Each row is one render device key, read with
  `get ini:Engine.Engine.RenderDevice <Key>` on open (empty reads as the High preset value, old `True` / `False` as
  High / 0) and written with `set ini:Engine.Engine.RenderDevice <Key> <Value>` on change: combos and sliders write a
  number, boxes `True|False`. A row changed by hand also writes `ModernPreset 4` (Custom). While the master is off
  every row is grey, and a change is undone.
- `ModernMenuGraphics` extends `ModernMenuPage`: the main page. `ModernGraphics` box (master), `ModernPreset` combo
  (0 Low, 1 Medium, 2 High, 3 Ultra, 4 Custom; Low to Ultra write every level key of the preset), rows that open the
  three pages below, Back.
- `ModernMenuLighting` extends `ModernMenuPage`: `ModernSunShadows` (0 Off to 4 Ultra), `ModernShadowDistance`
  (slider 1 to 5), `ModernLampShadows` (0 Off to 4 Ultra), `ModernLampSize` (slider 0 to 10; no preset writes it, so
  a change keeps the preset), `ModernContactShadows` (0 Off, 1 Low, 2 High), `ModernAO` (0 Off to 3 High),
  `ModernIndirect` (0 Off, 1 Low, 2 High).
- `ModernMenuImage` extends `ModernMenuPage`: `ModernAntiAliasing` (0 Off, 1 FXAA, 2 SMAA), `ModernBloom` and
  `ModernSharpen` (sliders 0 to 10).
- `ModernMenuEffects` extends `ModernMenuPage`: sliders 0 to 10 for `ModernGodRays`, `ModernVignette`,
  `ModernChromaticAberration` (0 off) and `ModernSaturation`, `ModernContrast`, `ModernExposure`
  (5 neutral). No preset writes them, and changing one keeps the preset.
- `ModernMenuSurfaces`, `ModernMenuParticles`, `ModernMenuScreen`, `ModernMenuMotion` extend `ModernMenuPage`: the
  rows of the sixteen upgrades, a switch for a top of 1, a
  combo for `ModernReflections`, `ModernSharpHud`, `ModernFireVolumes`, sliders 0 to 10 otherwise. Like the picture
  effects, no preset writes them and changing one keeps the preset. A box reads `True` as at least 1, so
  `ModernTemporalAA` (default 0) works as a box.

- `ModernPhotoMode` (an `Interaction` the host adds to the viewport) and `ModernPhotoCamera`: photo mode.
  F3 or the in-game menu's row opens it: the game pauses, a hidden camera
  flies, the photo page shows. H hides and shows the page; with it hidden the mouse turns the camera and a click
  takes a picture; Q and E tilt; Esc leaves. Opening (and Reset) copies the player's eight picture rows into their
  `ModernPhoto*` twins, sets `ModernPhotoFov`, `ModernPhotoFocus`, `ModernPhotoBlur` and `ModernPhotoHidePlayer` to 0
  and `ModernPhotoWarmth` to 5, and puts tilt, camera speed, grid and black bars back. The root window is told not to
  pause or resume around the page, so photo mode keeps the pause. It draws the black bars (in the picture) and the
  rule-of-thirds grid (never in the picture) on the canvas; "Focus on center" traces once along the view and writes
  the focus level nearest the hit (32 x 2^(level / 4) units).
- `ModernMenuPhoto` extends `ModernMenuPage`: what the two photo pages share, the page's rows, a row to the other page,
  then Take photo, Reset, Hide menu, Exit photo mode. Its sliders write the `ModernPhoto*` keys alone, never
  `ModernPreset`. It sits at the left edge; like every retail page it draws no backdrop (`bDarkenBackground` off, no
  client border), so the game shows behind it. `ModernMenuPhotoCamera`: field of view, tilt, orbit player, height,
  camera speed, focus on center, focus distance, background blur, hide player, grid, black bars, reset camera; the
  camera sliders follow the camera as it flies. `ModernMenuPhotoPicture`: the eight picture sliders and warmth.
- `ModernMenuGame` extends retail `Shell.MenuGame`: with modern graphics on and photo mode able to open, Super's
  Resume row becomes "Photo Mode" and a fresh Resume goes below it. The host swaps it in for `Shell.MenuGame`.

`ModernBake` keeps its key but has no row. Values and presets: `crates/p2-host/src/display.rs`.

## Building

Build time only: UCC and the `EditPackages` line are needed to compile, never to play. The built `P2Modern.u` is our own file and lives in git here, so a release needs no editor. A rebuild needs the POSTAL 2 editor: only its `System\UCC.exe`
compiles this package.

To build, from the install
(the folder that holds `System\UCC.exe`):

1. Copy this package folder next to `System`, so the sources sit at `..\P2Modern\Classes` seen from `System`. Move
   any earlier copy aside first: a class left in that folder compiles in too.
2. In `System\Postal2.ini`, add `EditPackages=P2Modern` at the end of the first `EditPackages` list.
3. From `System`, run `UCC.exe make`. UCC builds only packages whose `.u` is missing, so move any old
   `System\P2Modern.u` aside first.
4. Move the built `System\P2Modern.u` to `%APPDATA%\p2-reimplemented\Packages\`.
5. Take the `EditPackages=P2Modern` line out of `Postal2.ini` again (or put back a copy made before step 2).

