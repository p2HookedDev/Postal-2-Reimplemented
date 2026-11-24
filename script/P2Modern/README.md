# P2Modern: the Modern Graphics menu package

Our own UnrealScript package that adds a "Modern Graphics" row to the Options menu and three pages for the modern
graphics: a main page (master switch, quality preset), "Shadows and lighting" and "Image and effects", with combos,
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
  (slider 1 to 5), `ModernLampShadows` (0 Off to 4 Ultra), `ModernContactShadows` (0 Off, 1 Low, 2 High), `ModernAO`
  (0 Off to 3 High), `ModernIndirect` (0 Off, 1 Low, 2 High), `ModernTerrainDetail` box.
- `ModernMenuImage` extends `ModernMenuPage`: `ModernAntiAliasing` (0 Off, 1 FXAA, 2 SMAA, 3 MSAA 2x, 4 MSAA 4x),
  `ModernBloom` and `ModernSharpen` (sliders 0 to 10), `ModernRelief` and `ModernSpecular` boxes.
- `ModernMenuEffects` extends `ModernMenuPage`: sliders 0 to 10 for `ModernGodRays`, `ModernVignette`,
  `ModernChromaticAberration` (0 off) and `ModernSaturation`, `ModernContrast`, `ModernExposure`
  (5 neutral). No preset writes them, and changing one keeps the preset.

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

