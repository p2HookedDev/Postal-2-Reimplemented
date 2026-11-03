# P2Modern: the Modern Graphics menu package

Our own UnrealScript package that adds a "Modern Graphics" row to the Options menu and a page of checkboxes for
the modern graphics (master switch, sun shadows, contact shadows, ambient occlusion, surface relief, generated
specular, bloom and sharpening, smooth transitions). It is our code, not retail's; it uses only the stock Shell
menu helpers, so it looks like the retail pages.

## Classes

- `ModernMenuOptions` extends retail `Shell.MenuOptions`: the stock page, its last row (Back) relabelled
  "Modern Graphics", a fresh Back below it. The host swaps it in for `Shell.MenuOptions`.
- `ModernMenuGraphics` extends `Shell.ShellMenuCW`: one checkbox per render device key. Each reads with
  `get ini:Engine.Engine.RenderDevice <Key>` on open and writes `set ini:Engine.Engine.RenderDevice <Key> True|False`
  on change. Keys: `ModernGraphics` (master), `ModernShadows`, `ModernContactShadows`, `ModernAO`, `ModernRelief`,
  `ModernSpecular`, `ModernPost`, `ModernSmoothing`. Effect boxes are disabled and grey while the master is off.

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

