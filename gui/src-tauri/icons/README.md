# App icons

The desktop icon set (`icon.ico`, `icon.icns`, and the multi-resolution
PNGs listed in `tauri.conf.json` → `bundle.icon`) is generated from
`icon.png` — the 512×512 brand mark: the node graph on a rounded square
in the design system's CEC magenta (`oklch(0.64 0.255 350)`), matching
the allmystuff.works favicon. The `.ico` is also what `tauri-build`
embeds as the Windows
resource, so it must exist for a Windows build.

To regenerate from new source artwork (≥ 1024×1024 recommended):

```sh
cd gui
pnpm tauri icon path/to/allmystuff-1024.png
```

That rewrites every size Tauri needs back into this folder. The mobile /
Windows-Store assets it also emits (`android/`, `ios/`, `Square*Logo.png`,
`StoreLogo.png`) aren't used by the desktop bundle and aren't committed.

`icon-ios.png` is the full-bleed variant of the same mark (1024×1024,
opaque, the magenta field running edge to edge): iOS masks app icons with
its own superellipse, so the desktop mark's pre-rounded square and
transparent corners would render as a tile floating on a white plate.
`gui/mobile/patch-xcode-project.sh` stamps this one into the generated
Xcode asset catalog after every `tauri ios init`.
