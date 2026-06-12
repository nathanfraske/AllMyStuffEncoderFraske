<script lang="ts">
  // The call panel of a virtual room — what "joining" opens. It reads
  // like a muted call: members along the top, shared screens as tiles in
  // the middle, and a Zoom-style control strip at the bottom where
  // **everything starts off**. Each toggle fans ordinary routes out to
  // the members, so authorization (and the share sheet) applies the same
  // as wiring on the graph.
  //
  // The strip keeps two audio ideas deliberately apart:
  //   🎙 Mic — your voice to the room (the call itself);
  //   🔊 Share sound — what this *machine* is playing (its loopback),
  //      never your microphone.
  import { app } from "../store.svelte";
  import { displayName, isAppNode } from "../types";
  import RoomTile from "./RoomTile.svelte";

  const room = $derived(app.openRoom);
  let chatInput = $state("");
  let chatLog = $state<HTMLDivElement | null>(null);
  let filesPickerOpen = $state(false);
  // The owner's inline rename: the title flips to an input in place.
  let renaming = $state(false);
  let renameDraft = $state("");

  function startRename() {
    if (!room || !app.canRenameRoom(room)) return;
    renameDraft = room.name;
    renaming = true;
  }

  function commitRename() {
    if (!renaming) return;
    renaming = false;
    if (room) app.renameRoom(room.id, renameDraft);
  }

  const chat = $derived(room ? app.roomChat[room.id] ?? [] : []);
  const unread = $derived(room ? app.roomUnread[room.id] ?? 0 : 0);

  // The host's "invite more machines" picker.
  let invitePickerOpen = $state(false);
  const inviteCandidates = $derived(
    room
      ? app.roomCandidateNodes.filter(
          (n) => !room.members.some((m) => app.machineByAnyId(m)?.id === n.id),
        )
      : [],
  );

  function invite(nodeId: string) {
    if (!room) return;
    app.addRoomMembers(room.id, [nodeId]);
  }

  function inRoom(memberId: string): boolean {
    if (!room) return false;
    return (app.roomPresence[room.id] ?? []).some((m) => m === memberId);
  }

  function sendChat() {
    if (!chatInput.trim()) return;
    app.sendRoomChat(chatInput);
    chatInput = "";
  }

  function toggleChat() {
    app.roomChatOpen = !app.roomChatOpen;
    if (app.roomChatOpen && room) app.roomUnread = { ...app.roomUnread, [room.id]: 0 };
  }

  // New chat while the sidebar is open is read immediately; keep the log
  // pinned to the newest line. (The unread write is guarded — an effect
  // that unconditionally writes what it reads never settles.)
  $effect(() => {
    void chat.length;
    if (app.roomChatOpen && room) {
      if ((app.roomUnread[room.id] ?? 0) !== 0) {
        app.roomUnread = { ...app.roomUnread, [room.id]: 0 };
      }
      const el = chatLog;
      if (el) requestAnimationFrame(() => (el.scrollTop = el.scrollHeight));
    }
  });

  function sendFiles() {
    const targets = app.roomFileTargets;
    if (targets.length === 0) return;
    if (targets.length === 1) {
      app.openFiles(targets[0].id);
      return;
    }
    filesPickerOpen = !filesPickerOpen;
  }

  function timeOf(at: number): string {
    return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
</script>

{#if room}
  <div class="room-wrap">
    <section class="room" aria-label="Room {room.name}">
      <header class="head">
        {#if renaming}
          <!-- svelte-ignore a11y_autofocus — the input replaces the title
               the user just clicked; focusing it is the whole point. -->
          <input
            class="title-input"
            autofocus
            bind:value={renameDraft}
            onkeydown={(e) => {
              if (e.key === "Enter") commitRename();
              if (e.key === "Escape") renaming = false;
            }}
            onblur={commitRename}
          />
        {:else if app.canRenameRoom(room)}
          <button class="title title-btn" title="Rename this room (you made it)" onclick={startRename}>
            🪩 {room.name} <span class="pencil" aria-hidden="true">✎</span>
          </button>
        {:else}
          <div class="title">🪩 {room.name}</div>
        {/if}
        <span
          class="host-chip"
          class:mine={app.isRoomHost(room)}
          title="The room's identity, roster and name live with its host. Rooms are stream-only — nothing is stored, here or anywhere."
        >
          🏠 {app.isRoomHost(room) ? "you host this room" : `hosted by ${app.roomHostLabel(room)}`}
          · stream-only
        </span>
        <div class="members">
          {#each app.roomMemberNodes as m (m.id)}
            {@const n = m.node}
            <span class="member" class:here={inRoom(m.id)} class:offline={!!n && !n.online}>
              <span class="m-dot" class:on={!!n?.online}></span>
              {n ? displayName(n) : `${m.id.slice(0, 8)}…`}
              {#if n && inRoom(m.id)}
                {@const sends = app.roomMemberSends(n.id)}
                {#if sends.mic}<span title="Talking (their mic)">🎙</span>{/if}
                {#if sends.sound}<span title="Sharing their machine's sound">🔊</span>{/if}
              {/if}
              {#if n && !app.roomsSupported(n)}
                <span class="m-note" title="Runs an older AllMyStuff — they won't see invites or chat">old app</span>
              {:else if n && isAppNode(n) && n.relationship.kind === "unclaimed"}
                <span class="m-note" title="Claim it (or mark it shared) before media can route there">unclaimed</span>
              {/if}
              {#if app.isRoomHost(room)}
                <button
                  class="m-remove"
                  title="Remove this machine from the room"
                  aria-label="Remove member"
                  onclick={() => app.removeRoomMember(room.id, m.id)}>✕</button
                >
              {/if}
            </span>
          {:else}
            <span class="m-note solo">just you — invite a machine to share with</span>
          {/each}
          {#if app.isRoomHost(room)}
            <span class="invite-wrap">
              <button
                class="btn small"
                disabled={inviteCandidates.length === 0}
                title={inviteCandidates.length === 0
                  ? "Every machine on the graph is already in this room"
                  : "Invite more machines (you host this room)"}
                onclick={() => (invitePickerOpen = !invitePickerOpen)}>＋ Invite</button
              >
              {#if invitePickerOpen && inviteCandidates.length > 0}
                <div class="invite-pick">
                  {#each inviteCandidates as n (n.id)}
                    <button
                      class="invite-row"
                      onclick={() => {
                        invitePickerOpen = false;
                        invite(n.id);
                      }}
                    >
                      <span class="m-dot" class:on={n.online}></span>
                      {displayName(n)}
                    </button>
                  {/each}
                </div>
              {/if}
            </span>
          {/if}
        </div>
        <button class="btn small" class:primary={unread > 0} onclick={toggleChat}>
          💬 Chat{#if unread > 0}&nbsp;<b>{unread}</b>{/if}
        </button>
        <button class="btn small danger-btn" onclick={() => app.leaveRoom(room.id)}>Leave room</button>
        <button
          class="btn small ghost mini"
          title="Back to the graph — you stay in the room (everything keeps streaming)"
          aria-label="Minimize the room"
          onclick={() => app.closeRoomPanel()}>—</button
        >
      </header>

      <div class="body">
        <div class="stage" class:with-chat={app.roomChatOpen}>
          {#if app.roomInboundShares.length > 0}
            <div class="tiles" class:single={app.roomInboundShares.length === 1}>
              {#each app.roomInboundShares as share (share.route.id)}
                <RoomTile route={share.route} member={share.member} />
              {/each}
            </div>
          {:else}
            <div class="empty">
              <div class="empty-orb">🪩</div>
              <div class="empty-title">You're in — and sending nothing.</div>
              <div class="empty-sub">
                Mic, camera and screen are off until you turn them on below. Shared screens from
                other members show up here. Sharing in a room is scoped to the room — it never
                adds standing permissions for its members.
              </div>
            </div>
          {/if}
        </div>

        {#if app.roomChatOpen}
          <aside class="chat">
            <div class="chat-log" bind:this={chatLog}>
              {#each chat as line, i (i)}
                <div class="line" class:mine={app.isMe(line.from)}>
                  <span class="line-who">{app.isMe(line.from) ? "You" : line.fromLabel}</span>
                  <span class="line-text">{line.text}</span>
                  <span class="line-at">{timeOf(line.at)}</span>
                </div>
              {:else}
                <p class="chat-empty">
                  No messages yet.{#if !app.backendConnected}&nbsp;(Demo mode — chat stays on this
                    device.){/if}
                </p>
              {/each}
            </div>
            <div class="chat-send">
              <input
                placeholder="Message the room…"
                bind:value={chatInput}
                onkeydown={(e) => e.key === "Enter" && sendChat()}
              />
              <button class="btn small primary" onclick={sendChat}>Send</button>
            </div>
          </aside>
        {/if}
      </div>

      <footer class="strip">
        <div class="ctl-group">
          <button
            class="ctl"
            class:on={app.roomMic}
            onclick={() => app.toggleRoomMic()}
            title="Talk to the room — your microphone to the members' speakers"
          >
            <span class="ctl-icon">{app.roomMic ? "🎙" : "🎙"}</span>
            <span class="ctl-label">Mic</span>
            <span class="ctl-sub">{app.roomMic ? "on — talking" : "off"}</span>
          </button>
          <button
            class="ctl"
            class:on={app.roomCam}
            onclick={() => app.toggleRoomCam()}
            title="Send your camera to the room (camera transport is on its way)"
          >
            <span class="ctl-icon">📷</span>
            <span class="ctl-label">Camera</span>
            <span class="ctl-sub">{app.roomCam ? "on" : "off"}</span>
          </button>
          <button
            class="ctl"
            class:on={app.roomScreen}
            onclick={() => app.toggleRoomScreen()}
            title="Share this screen with the room"
          >
            <span class="ctl-icon">🖥</span>
            <span class="ctl-label">Share screen</span>
            <span class="ctl-sub">{app.roomScreen ? "sharing" : "off"}</span>
          </button>
        </div>

        <div class="ctl-sep"></div>

        <div class="ctl-group">
          <button
            class="ctl"
            class:on={app.roomSound}
            onclick={() => app.toggleRoomSound()}
            title="Share what this machine is playing (its system audio) — NOT your microphone"
          >
            <span class="ctl-icon">🔊</span>
            <span class="ctl-label">Share sound</span>
            <span class="ctl-sub">{app.roomSound ? "sharing audio" : "this machine's audio"}</span>
          </button>
          <button
            class="ctl"
            class:on={app.roomControl}
            onclick={() => app.toggleRoomControl()}
            title="Let members click and type on this machine (owner/fleet members only — others' input is dropped)"
          >
            <span class="ctl-icon">🕹</span>
            <span class="ctl-label">Share control</span>
            <span class="ctl-sub">{app.roomControl ? "members can drive" : "off"}</span>
          </button>
          <div class="files-wrap">
            <button
              class="ctl"
              disabled={app.roomFileTargets.length === 0}
              onclick={sendFiles}
              title={app.roomFileTargets.length === 0
                ? "File sending is owner/fleet only — no eligible member right now"
                : "Send files to a member (opens the file manager)"}
            >
              <span class="ctl-icon">🗂</span>
              <span class="ctl-label">Send files</span>
              <span class="ctl-sub">{app.roomFileTargets.length || "no"} member{app.roomFileTargets.length === 1 ? "" : "s"}</span>
            </button>
            {#if filesPickerOpen && app.roomFileTargets.length > 1}
              <div class="files-pick">
                {#each app.roomFileTargets as t (t.id)}
                  <button
                    class="files-pick-row"
                    onclick={() => {
                      filesPickerOpen = false;
                      app.openFiles(t.id);
                    }}>🗂 {displayName(t)}</button
                  >
                {/each}
              </div>
            {/if}
          </div>
        </div>

        <p class="clarity">
          🎙 <b>Mic</b> is the call — your voice. 🔊 <b>Share sound</b> sends what this machine is
          <i>playing</i> — never your mic. Everything here is <b>scoped to the room</b> and
          <b>stream-only</b>: nothing is stored, it all ends when you leave, and nobody gains a
          standing permission from it.
        </p>
      </footer>
    </section>
  </div>
{/if}

<style>
  .room-wrap {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 1.2rem;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(3px);
    z-index: 25;
    animation: fade 0.12s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  .room {
    width: min(72rem, 100%);
    height: min(46rem, 100%);
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    padding: 0.65rem 0.9rem;
    border-bottom: 1px solid var(--line);
    flex-wrap: wrap;
  }
  .title {
    font-weight: 750;
    font-size: 1rem;
    white-space: nowrap;
  }
  .title-btn {
    border: none;
    background: transparent;
    color: var(--ink);
    padding: 0.1rem 0.3rem;
    border-radius: var(--r-sm);
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
  }
  .title-btn:hover {
    background: var(--surface-2);
  }
  .pencil {
    font-size: 0.78rem;
    color: var(--ink-faint);
  }
  .title-btn:hover .pencil {
    color: var(--ink);
  }
  .title-input {
    font-weight: 750;
    font-size: 1rem;
    font-family: inherit;
    color: var(--ink);
    background: var(--surface-2);
    border: 1px solid var(--accent);
    border-radius: var(--r-sm);
    padding: 0.15rem 0.45rem;
    min-width: 12rem;
  }
  .host-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.7rem;
    font-weight: 650;
    color: var(--ink-faint);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r-pill);
    padding: 0.16rem 0.55rem;
    white-space: nowrap;
  }
  .host-chip.mine {
    color: var(--accent-ink);
    background: var(--accent-soft);
    border-color: oklch(0.64 0.255 350 / 0.35);
  }
  .members {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex: 1;
    min-width: 0;
    flex-wrap: wrap;
  }
  .m-remove {
    border: none;
    background: transparent;
    color: var(--ink-faint);
    font-size: 0.66rem;
    padding: 0 0.1rem;
    line-height: 1;
  }
  .m-remove:hover {
    color: var(--danger);
  }
  .m-note.solo {
    color: var(--ink-faint);
    font-size: 0.74rem;
  }
  .invite-wrap {
    position: relative;
    display: inline-flex;
  }
  .invite-pick {
    position: absolute;
    top: calc(100% + 0.35rem);
    left: 0;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-md);
    padding: 0.3rem;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 12rem;
    z-index: 6;
  }
  .invite-row {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    border: none;
    background: transparent;
    color: var(--ink);
    text-align: left;
    font-size: 0.78rem;
    padding: 0.35rem 0.5rem;
    border-radius: var(--r-sm);
  }
  .invite-row:hover {
    background: var(--surface-2);
  }
  .mini {
    font-weight: 800;
  }
  .member {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.74rem;
    font-weight: 600;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r-pill);
    padding: 0.16rem 0.55rem;
    color: var(--ink-soft);
  }
  .member.here {
    border-color: oklch(0.8 0.17 150 / 0.4);
    color: var(--ink);
  }
  .member.offline {
    opacity: 0.55;
  }
  .m-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--line-strong);
  }
  .m-dot.on {
    background: var(--ok);
  }
  .m-note {
    font-size: 0.62rem;
    color: var(--warn);
  }
  .danger-btn {
    color: var(--danger);
    border-color: oklch(0.7 0.19 14 / 0.4);
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .stage {
    flex: 1;
    min-width: 0;
    padding: 0.8rem;
    overflow: auto;
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
    gap: 0.7rem;
  }
  .tiles.single {
    grid-template-columns: 1fr;
    height: 100%;
  }
  .tiles.single :global(.tile) {
    height: 100%;
  }
  .empty {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.35rem;
    text-align: center;
  }
  .empty-orb {
    font-size: 2.4rem;
    filter: drop-shadow(0 3px 6px oklch(0.64 0.255 350 / 0.35));
  }
  .empty-title {
    font-weight: 750;
  }
  .empty-sub {
    font-size: 0.82rem;
    color: var(--ink-faint);
    max-width: 26rem;
  }
  .chat {
    width: 17rem;
    border-left: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    background: var(--surface-2);
  }
  .chat-log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0.6rem;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }
  .line {
    font-size: 0.8rem;
    line-height: 1.35;
  }
  .line-who {
    font-weight: 700;
    margin-right: 0.35rem;
  }
  .line.mine .line-who {
    color: var(--accent-ink);
  }
  .line-at {
    margin-left: 0.35rem;
    font-size: 0.66rem;
    color: var(--ink-faint);
  }
  .chat-empty {
    font-size: 0.76rem;
    color: var(--ink-faint);
    text-align: center;
    margin-top: 1rem;
  }
  .chat-send {
    display: flex;
    gap: 0.4rem;
    padding: 0.5rem;
    border-top: 1px solid var(--line);
  }
  .chat-send input {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-pill);
    padding: 0.35rem 0.7rem;
    font-size: 0.8rem;
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
  }
  .strip {
    border-top: 1px solid var(--line);
    padding: 0.55rem 0.9rem 0.5rem;
    display: flex;
    align-items: stretch;
    gap: 0.7rem;
    flex-wrap: wrap;
  }
  .ctl-group {
    display: flex;
    gap: 0.4rem;
  }
  .ctl-sep {
    width: 1px;
    background: var(--line);
    margin: 0.2rem 0;
  }
  .ctl {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1rem;
    min-width: 6.4rem;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    color: var(--ink);
    border-radius: var(--r-md);
    padding: 0.4rem 0.55rem 0.35rem;
    transition: border-color 0.12s ease, background 0.12s ease;
  }
  .ctl:hover:not(:disabled) {
    background: var(--surface-2);
  }
  .ctl:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .ctl.on {
    border-color: var(--ok);
    background: var(--ok-soft);
  }
  .ctl-icon {
    font-size: 1.05rem;
    line-height: 1;
  }
  .ctl-label {
    font-size: 0.74rem;
    font-weight: 700;
  }
  .ctl-sub {
    font-size: 0.62rem;
    color: var(--ink-faint);
  }
  .ctl.on .ctl-sub {
    color: var(--ok);
  }
  .files-wrap {
    position: relative;
  }
  .files-pick {
    position: absolute;
    bottom: calc(100% + 0.35rem);
    left: 0;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-md);
    padding: 0.3rem;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 11rem;
    z-index: 5;
  }
  .files-pick-row {
    border: none;
    background: transparent;
    color: var(--ink);
    text-align: left;
    font-size: 0.78rem;
    padding: 0.35rem 0.5rem;
    border-radius: var(--r-sm);
  }
  .files-pick-row:hover {
    background: var(--surface-2);
  }
  .clarity {
    flex-basis: 100%;
    margin: 0.1rem 0 0;
    font-size: 0.7rem;
    color: var(--ink-faint);
  }
</style>
