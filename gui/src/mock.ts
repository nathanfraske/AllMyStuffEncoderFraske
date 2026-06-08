// A believable starter graph so the app is alive the moment it opens —
// before any mesh is joined or any scan has run. The Tauri backend
// replaces `this` with a real scan of the machine and the peers with real
// presence adverts; the shapes are identical, so nothing downstream knows
// the difference.

import type { Capability, Catalog, Flow, MediaKind, MeshNode } from "./types";

function cap(
  node: string,
  id: string,
  label: string,
  media: MediaKind,
  flow: Flow,
  origin: string,
): Capability {
  return { id: `${node}:${id}`, node, label, media, flow, origin };
}

/** The synthetic "the machine itself" trio every node exposes. */
function machineCaps(node: string): Capability[] {
  return [
    cap(node, "screen", "Screen", "display", "source", "screen"),
    cap(node, "control", "Keyboard & mouse", "input", "sink", "control"),
    cap(node, "system-audio", "System audio", "audio", "duplex", "system"),
  ];
}

export function demoCatalog(): Catalog {
  const nodes: MeshNode[] = [
    {
      id: "this",
      label: "My MacBook",
      kind: "this",
      relationship: { kind: "mine" },
      online: true,
      summary: { os: "macOS 14", cpu: "Apple M2", ram_bytes: 16 * 2 ** 30, device_count: 13 },
    },
    {
      id: "desk",
      label: "Desk PC",
      kind: "machine",
      relationship: { kind: "mine" },
      online: true,
      summary: { os: "Windows 11", cpu: "Ryzen 7 7700", ram_bytes: 32 * 2 ** 30, device_count: 11 },
    },
    {
      id: "tv",
      label: "Living-room TV",
      kind: "machine",
      relationship: { kind: "mine" },
      online: true,
      summary: { os: "Linux", cpu: "Amlogic S905", ram_bytes: 4 * 2 ** 30, device_count: 5 },
    },
    {
      id: "studio",
      label: "Conference puck",
      kind: "machine",
      relationship: { kind: "mine" },
      online: true,
      summary: { os: "Linux", cpu: "Pi 5", ram_bytes: 8 * 2 ** 30, device_count: 6 },
    },
    {
      id: "alex",
      label: "Alex's laptop",
      kind: "machine",
      online: false,
      relationship: {
        kind: "shared",
        person: { id: "person:alex", name: "Alex" },
        // One grant pre-set so a share already works; video is left
        // ungranted so connecting Alex's camera shows the ask-permission
        // flow.
        grants: [
          {
            id: "g-screen",
            media: "display",
            role: "consume",
            capability: null,
            label: "Receive your screen",
          },
        ],
      },
    },
  ];

  const capabilities: Capability[] = [
    // My MacBook.
    ...machineCaps("this"),
    cap("this", "mic", "MacBook mic array", "audio", "source", "microphone"),
    cap("this", "speakers", "MacBook speakers", "audio", "sink", "speaker"),
    cap("this", "cam", "FaceTime HD camera", "video", "source", "camera"),
    cap("this", "display", "Built-in Retina display", "display", "sink", "display"),
    cap("this", "keyboard", "Magic Keyboard", "input", "source", "keyboard"),
    cap("this", "trackpad", "Trackpad", "input", "source", "touchpad"),
    // Desk PC.
    ...machineCaps("desk"),
    cap("desk", "monitor", "Dell U2720Q", "display", "sink", "display"),
    cap("desk", "speakers", "Desk speakers", "audio", "sink", "speaker"),
    cap("desk", "yeti", "Blue Yeti", "audio", "source", "microphone"),
    cap("desk", "data", "Data (D:)", "storage", "duplex", "storage"),
    // Living-room TV.
    ...machineCaps("tv"),
    cap("tv", "oled", "LG OLED", "display", "sink", "display"),
    cap("tv", "soundbar", "Sonos soundbar", "audio", "sink", "speaker"),
    // Conference puck (the 6-mic array).
    ...machineCaps("studio"),
    cap("studio", "array", "ReSpeaker 6-mic array", "audio", "source", "microphone"),
    cap("studio", "speaker", "Puck speaker", "audio", "sink", "speaker"),
    // Alex (shared).
    ...machineCaps("alex"),
    cap("alex", "cam", "Alex's webcam", "video", "source", "camera"),
    cap("alex", "monitor", "Alex's monitor", "display", "sink", "display"),
    cap("alex", "mic", "Alex's headset mic", "audio", "source", "microphone"),
  ];

  // A few pre-wired routes (all between things I own, so all valid).
  const routes = [
    { id: "route:this:system-audio→desk:speakers", from: "this:system-audio", to: "desk:speakers", media: "audio" as MediaKind, group: null },
    { id: "route:desk:screen→tv:oled", from: "desk:screen", to: "tv:oled", media: "display" as MediaKind, group: null },
  ];

  // The RDC bundle: my MacBook's screen + keyboard + trackpad + mic +
  // speakers, ready to point at any machine as a unit.
  const groups = [
    {
      id: "group:my-desk",
      name: "My desk",
      node: "this",
      members: ["this:display", "this:keyboard", "this:trackpad", "this:mic", "this:speakers"],
    },
  ];

  return { nodes, capabilities, routes, groups };
}
