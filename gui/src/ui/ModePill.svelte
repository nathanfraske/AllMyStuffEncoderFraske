<script lang="ts">
  // The stream-posture control, shared by the console strip and the
  // popped-out video bar so there is ONE Mode element, not two that drift
  // apart. Balanced (stability-first default), Game (latency-first —
  // GDR, instant recovery), Studio (LAN fidelity), Studio · LL
  // (bit-exact HEVC on NVIDIA, degrades to Studio elsewhere). The Studio
  // flavors warn once about bandwidth before engaging.
  //
  // Presentational + self-contained: the caller owns where the tune
  // lives and how it's applied; this component only decides which mode
  // is next and hands it back through `onapply`.

  type ModeKey = "balanced" | "game" | "studio" | "studio-ll";
  const MODES: ModeKey[] = ["balanced", "game", "studio", "studio-ll"];
  const MODE_LABEL: Record<ModeKey, string> = {
    balanced: "Balanced",
    game: "Game",
    studio: "Studio",
    "studio-ll": "Studio · LL",
  };

  /** The posture as it rides the wire (matches StreamTune["mode"] minus
   *  the "balanced" alias, which is expressed as undefined). */
  type WireMode = "game" | "studio" | "studio-lossless" | undefined;

  let {
    mode,
    game,
    onapply,
  }: {
    /** The current wire posture ("studio-lossless" for the LL flavor). */
    mode: string | undefined;
    /** Legacy game flag, honored when no named mode is set. */
    game?: boolean;
    /** Apply a chosen posture. `wireMode` is undefined for Balanced,
     *  "studio-lossless" for LL, else the key; `gameFlag` mirrors it for
     *  hosts that predate the tri-state. */
    onapply: (wireMode: WireMode, gameFlag: boolean | undefined) => void;
  } = $props();

  const STUDIO_ACK_KEY = "ams.studioBandwidthAck";
  let studioPrompt = $state<ModeKey | null>(null);

  const modeKey = (): ModeKey =>
    mode === "studio-lossless"
      ? "studio-ll"
      : ((mode as ModeKey | undefined) ?? (game ? "game" : "balanced"));

  function apply(next: ModeKey) {
    const wire: WireMode =
      next === "balanced" ? undefined : next === "studio-ll" ? "studio-lossless" : next;
    onapply(wire, next === "game" ? true : undefined);
  }
  function cycle() {
    const next = MODES[(MODES.indexOf(modeKey()) + 1) % MODES.length];
    if (
      (next === "studio" || next === "studio-ll") &&
      localStorage.getItem(STUDIO_ACK_KEY) !== "1"
    ) {
      studioPrompt = next;
      return;
    }
    apply(next);
  }
  function confirmStudio(dontAskAgain: boolean) {
    if (dontAskAgain) localStorage.setItem(STUDIO_ACK_KEY, "1");
    const next = studioPrompt ?? "studio";
    studioPrompt = null;
    apply(next);
  }
</script>

<button
  class="mode-pill"
  class:tuned={modeKey() !== "balanced"}
  title="Balanced favors stability and quality; Game favors latency and instant recovery; Studio spends LAN bandwidth on fidelity; Studio · LL is bit-exact (NVIDIA both ends)"
  onpointerdown={(e) => e.stopPropagation()}
  onpointerup={(e) => e.stopPropagation()}
  onclick={(e) => {
    e.stopPropagation();
    cycle();
  }}
>
  Mode · {MODE_LABEL[modeKey()]}
</button>

{#if studioPrompt}
  <div
    class="studio-scrim"
    role="presentation"
    onpointerdown={(e) => e.stopPropagation()}
    onclick={() => (studioPrompt = null)}
    onkeydown={(e) => e.key === "Escape" && (studioPrompt = null)}
  >
    <div
      class="studio-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="studio-title"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
    >
      <h3 id="studio-title">
        {studioPrompt === "studio-ll" ? "Turn on Studio · Lossless?" : "Turn on Studio mode?"}
      </h3>
      {#if studioPrompt === "studio-ll"}
        <p>
          Lossless sends <strong>every pixel exactly</strong> over HEVC —
          bandwidth follows what's on screen: near-zero when idle, tens of
          Mbps for desktop work, and it can spike far higher on busy video.
          It needs NVIDIA hardware on both machines; anywhere it can't run,
          the stream continues as regular Studio automatically.
        </p>
      {/if}
      <p>
        Studio streams at maximum fidelity and can use <strong>150 Mbps
        and up</strong> — it's built for a fast local network. It runs
        wherever you turn it on, so on a slow or metered connection expect
        stutter until you lower the Rate.
      </p>
      <div class="studio-actions">
        <button class="studio-btn ghost" onclick={() => (studioPrompt = null)}>Cancel</button>
        <button class="studio-btn ghost" onclick={() => confirmStudio(true)}>Don't ask again</button
        >
        <button class="studio-btn primary" onclick={() => confirmStudio(false)}
          >{studioPrompt === "studio-ll" ? "Use Lossless" : "Use Studio"}</button
        >
      </div>
    </div>
  </div>
{/if}

<style>
  .mode-pill {
    border: 1px solid rgba(255, 255, 255, 0.2);
    background: rgba(0, 0, 0, 0.55);
    color: #e7e2fa;
    border-radius: var(--r-pill, 999px);
    padding: 0.28rem 0.65rem;
    font-size: 0.74rem;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
  }
  .mode-pill:hover {
    border-color: var(--accent, #7c6cf0);
  }
  .mode-pill.tuned {
    border-color: var(--accent, #7c6cf0);
    color: #fff;
  }
  .studio-scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(2px);
  }
  .studio-dialog {
    width: min(30rem, calc(100vw - 3rem));
    background: #16181d;
    color: #e8ebf0;
    border: 1px solid #2c323b;
    border-radius: 10px;
    padding: 1.25rem 1.35rem 1.1rem;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
  }
  .studio-dialog h3 {
    margin: 0 0 0.5rem;
    font-size: 1.05rem;
    font-weight: 640;
  }
  .studio-dialog p {
    margin: 0 0 1.1rem;
    font-size: 0.9rem;
    line-height: 1.5;
    color: #b6bdc8;
  }
  .studio-dialog strong {
    color: #e8ebf0;
    font-variant-numeric: tabular-nums;
  }
  .studio-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .studio-btn {
    padding: 0.45rem 0.85rem;
    border-radius: 7px;
    font-size: 0.85rem;
    font-weight: 560;
    cursor: pointer;
    border: 1px solid transparent;
  }
  .studio-btn.ghost {
    background: transparent;
    border-color: #363d47;
    color: #c4cbd6;
  }
  .studio-btn.ghost:hover {
    border-color: #4a525e;
    color: #e8ebf0;
  }
  .studio-btn.primary {
    background: #2f6fab;
    color: #fff;
  }
  .studio-btn.primary:hover {
    background: #3a7cbb;
  }
  .studio-btn:focus-visible {
    outline: 2px solid #5b96cf;
    outline-offset: 2px;
  }
</style>
