# App icons

`icon.png` is a 512×512 placeholder (a little node graph in the AllMyStuff
accent). Before cutting a release, generate the full platform set —
`.ico`, `.icns`, and the multi-resolution PNGs — from a final source
artwork:

```sh
cd gui
pnpm tauri icon path/to/allmystuff-1024.png
```

That writes every size Tauri needs back into this folder; then list them
in `tauri.conf.json` under `bundle.icon`.
