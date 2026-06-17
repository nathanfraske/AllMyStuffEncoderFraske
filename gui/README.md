# AllMyStuff GUI

Tauri 2 + Svelte 5. The friendly graph that scans your machine and wires
your stuff together. A **client** of the `myownmesh` daemon — it never
embeds the engine.

## Layout

```
gui/
├── src/                      # Svelte 5 front-end
│   ├── ui/
│   │   ├── App.svelte        # shell: top bar + stage + sheets
│   │   ├── Graph.svelte      # the node graph (SVG edges + HTML node cards, shared pan/zoom)
│   │   ├── NodeDrawer.svelte # click a node → stats, devices, connections, sharing
│   │   ├── RoomsBar.svelte   # start/join virtual rooms; RoomPanel/RoomTile/RoomHost run a call
│   │   ├── ClaimSheet.svelte # the "mine vs sharing" decision (with ShareSheet)
│   │   ├── ShareSheet.svelte # the one-tap permission moment
│   │   └── Toasts.svelte
│   ├── types.ts              # TS mirror of the graph model + visual helpers
│   ├── catalog.ts            # routing + authorization rules (port of allmystuff-graph)
│   ├── store.svelte.ts       # app state (runes) + the connect/share/room verbs (fleet + rooms)
│   ├── mock.ts               # demo graph so the app is alive with no backend
│   └── tauri.ts              # backend bridge (degrades to web mode)
└── src-tauri/                # Tauri (Rust) shell — its own Cargo workspace
    └── src/main.rs           # commands (scan_self, mesh_*, update_*) + a TauriSink
                              # over the allmystuff-node engine (../../node)
```

The mesh node itself — the control-socket transport, the daemon spawner, and
every media plane — lives in the **`allmystuff-node`** crate (`../node`), which
this shell links and `allmystuff serve` runs headless. `src-tauri` is just the
webview wiring on top.

## Develop

```sh
pnpm install
pnpm check        # svelte-check (types)
pnpm build        # vite production build (no webview needed)
pnpm tauri dev    # full desktop app — needs the Linux webview deps + a daemon
```

The front-end is the part that builds anywhere. The Tauri backend links the
system webview, so building the desktop app on Linux needs `libgtk-3-dev` and
`libwebkit2gtk-4.1-dev`; macOS and Windows use their built-in webviews.

## Web mode vs desktop

`tauri.ts` detects whether a Tauri backend is present. With one, `scan_self`
replaces the demo "this device" with a real scan and the mesh commands light
up. Without one (a plain `pnpm dev` in a browser, or this repo's CI build),
the demo graph stands in so the whole experience — clicking nodes, drawing
connections, the share sheet, rooms — works offline.

## Backend events

The backend pumps the daemon's event stream out as the `allmystuff://event`
Tauri event and tracks connection state under `allmystuff://subscription`
(query `mesh_subscription_state` after registering the listener to avoid the
fire-and-forget race — the MyOwnMesh GUI pattern).
