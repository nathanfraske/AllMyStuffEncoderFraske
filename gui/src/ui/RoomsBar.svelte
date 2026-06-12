<script lang="ts">
  // Virtual rooms — the zoom-like calls between your machines (and the
  // people you share with). This bar lists every room this device is
  // entitled to: rooms made here, rooms it's been invited into (past
  // invites stay listed like roster slots, until the host removes this
  // device or closes the room). New rooms are made here too — open
  // (anyone with the id walks in) or invite-only (the host admits each
  // knock) — and an id someone passed you can be pasted to ask in.
  // Joining opens the call: its own OS window on the desktop, the
  // in-page panel (RoomPanel) in the web preview.
  import { app } from "../store.svelte";
  import type { RoomAccess } from "../types";

  let draftName = $state("");
  let draftMembers = $state<string[]>([]);
  let draftAccess = $state<RoomAccess>("invite");

  // The "paste an id you were given" affordance.
  let joinOpen = $state(false);
  let joinCode = $state("");

  function toggleMember(id: string) {
    draftMembers = draftMembers.includes(id)
      ? draftMembers.filter((m) => m !== id)
      : [...draftMembers, id];
  }

  function create() {
    app.createRoom(draftName, draftMembers, draftAccess);
    draftName = "";
    draftMembers = [];
    draftAccess = "invite";
  }

  function openDraft() {
    // Pre-fill the default so the maker sees what they'll get: a room
    // named after the fleet's owner ("Casey's room"). Still editable.
    draftName = app.defaultRoomName();
    draftMembers = [];
    draftAccess = "invite";
    joinOpen = false;
    app.roomDraftOpen = true;
  }

  async function knock() {
    if (await app.knockRoom(joinCode)) {
      joinCode = "";
      joinOpen = false;
    }
  }

  function memberSummary(ids: string[]): string {
    const names = [
      ...new Set(ids.filter((id) => !app.isMe(id)).map((id) => app.roomWho(id).who)),
    ].filter(Boolean);
    const shown = names.slice(0, 3).join(", ");
    return names.length > 3 ? `${shown}, +${names.length - 3}` : shown || "just you";
  }

  function presentCount(roomId: string): number {
    return (app.roomPresence[roomId] ?? []).filter((m) => !app.isMe(m)).length;
  }
</script>

<div class="bar">
  <div class="bar-head">
    <h4>Rooms</h4>
    <div class="bar-actions">
      {#if app.roomDraftOpen}
        <button class="btn small" onclick={() => (app.roomDraftOpen = false)}>Close</button>
      {:else}
        <button
          class="btn small ghost"
          class:lit={joinOpen}
          title="Paste a room id someone gave you and ask to join"
          onclick={() => (joinOpen = !joinOpen)}>⌁ Join with an id</button
        >
        <button class="btn small primary" onclick={openDraft}>+ New room</button>
      {/if}
    </div>
  </div>

  {#if joinOpen && !app.roomDraftOpen}
    <div class="join-code">
      <input
        placeholder="room:…  (paste an id you were given)"
        bind:value={joinCode}
        onkeydown={(e) => e.key === "Enter" && knock()}
      />
      <button class="btn small primary" disabled={!joinCode.trim()} onclick={knock}>Ask in</button>
      <p class="hint faint">
        An <b>open</b> room lets you straight in; an invite-only host gets asked and can admit
        you.
      </p>
    </div>
  {/if}

  {#if app.roomDraftOpen}
    <!-- Making a room: name it, choose how it admits, pick who's in it. -->
    <div class="draft">
      <p class="hint">
        A room is a call between machines, and <b>you host the rooms you make</b> — their
        identity and roster are this device's. Start one with just this node and invite
        machines later.
      </p>
      <input
        class="name-input"
        placeholder={app.defaultRoomName()}
        bind:value={draftName}
        onkeydown={(e) => e.key === "Enter" && create()}
      />
      <div class="access-pick" role="radiogroup" aria-label="Who can join">
        <button
          class="access-opt"
          class:sel={draftAccess === "invite"}
          role="radio"
          aria-checked={draftAccess === "invite"}
          onclick={() => (draftAccess = "invite")}
        >
          <b>🔒 Invite</b>
          <span>only machines you invite; a pasted id knocks and you admit it</span>
        </button>
        <button
          class="access-opt"
          class:sel={draftAccess === "open"}
          role="radio"
          aria-checked={draftAccess === "open"}
          onclick={() => (draftAccess = "open")}
        >
          <b>🔓 Open</b>
          <span>anyone on a shared network with the room's id walks right in</span>
        </button>
      </div>
      <div class="cands">
        {#each app.roomCandidateNodes as n (n.id)}
          {@const w = app.roomWho(n.id)}
          <label class="cand">
            <input
              type="checkbox"
              checked={draftMembers.includes(n.id)}
              onchange={() => toggleMember(n.id)}
            />
            <span class="cand-name">{w.who}{#if w.machine}&nbsp;<span class="cand-machine">· {w.machine}</span>{/if}</span>
            <span class="dot" class:on={n.online}></span>
          </label>
        {/each}
        {#if app.roomCandidateNodes.length === 0}
          <p class="hint faint">No other machines on the graph yet.</p>
        {/if}
      </div>
      <button class="btn primary small wide" onclick={create}>
        {draftMembers.length === 0 ? "Create room (just this node)" : "Create room"}
      </button>
    </div>
  {:else if app.rooms.length === 0}
    <p class="hint">
      No rooms yet. Make one to share your screen, sound or files with several machines at once.
    </p>
  {/if}

  {#if app.rooms.length > 0 && !app.roomDraftOpen}
    <ul class="rooms">
      {#each app.rooms as r (r.id)}
        <!-- A room being read in its own window owns its unread badge —
             this bar would otherwise count lines the user is looking at. -->
        {@const unread = app.isJoinedAnywhere(r.id) && !app.isJoined(r.id) ? 0 : app.roomUnread[r.id] ?? 0}
        {@const present = presentCount(r.id)}
        {@const knocks = (app.roomKnocks[r.id] ?? []).length}
        {@const joined = app.isJoinedAnywhere(r.id)}
        <li class:open={app.roomOpenId === r.id || (joined && !app.isJoined(r.id))}>
          <div class="r-main">
            <div class="r-name">
              🪩 {r.name}
              {#if app.roomAccess(r) === "open"}<span class="r-open" title="Open room — anyone with its id can join">🔓</span>{/if}
              {#if joined}<span class="in-dot" title="You're in this room"></span>{/if}
              {#if unread > 0}<span class="unread" title="Unread chat">{unread}</span>{/if}
              {#if knocks > 0}<span class="knock-badge" title="{knocks} asking to join — open the room to admit them">{knocks} asking</span>{/if}
            </div>
            <div class="r-sub">
              {app.isRoomHost(r) ? "hosted by you" : `hosted by ${app.roomHostLabel(r)}`}
              · {memberSummary(r.members)}{present > 0 ? ` · ${present} in now` : ""}
            </div>
          </div>
          {#if joined}
            {#if app.roomOpenId !== r.id}
              <button class="btn small primary" onclick={() => app.joinRoom(r.id)}>Open</button>
            {/if}
            <button class="btn small" onclick={() => app.leaveRoomEverywhere(r.id)}>Leave</button>
          {:else}
            <button class="btn small primary" onclick={() => app.joinRoom(r.id)}>Join</button>
          {/if}
          <button
            class="btn small ghost x"
            title={app.isRoomHost(r)
              ? "Close this room — you host it, so it ends for everyone"
              : "Remove this room from this device"}
            aria-label={app.isRoomHost(r) ? "Close room" : "Remove room"}
            onclick={() => app.deleteRoom(r.id)}>✕</button
          >
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .bar {
    position: absolute;
    left: 1rem;
    bottom: 1rem;
    width: 21rem;
    max-width: calc(100vw - 2rem);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-md);
    padding: 0.7rem 0.8rem 0.8rem;
    z-index: 15;
  }
  .bar-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.4rem;
  }
  .bar-actions {
    display: flex;
    gap: 0.3rem;
  }
  .bar-actions .lit {
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  h4 {
    margin: 0;
    font-size: 0.82rem;
    color: var(--ink-soft);
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0 0 0.5rem;
    line-height: 1.4;
  }
  .hint.faint {
    color: var(--ink-faint);
  }
  .join-code {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    margin-bottom: 0.6rem;
  }
  .join-code input {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.35rem 0.5rem;
    font-size: 0.76rem;
    font-family: var(--mono);
    background: var(--surface);
    color: var(--ink);
  }
  .join-code .hint {
    flex-basis: 100%;
    margin: 0;
  }
  .name-input {
    width: 100%;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.5rem;
    font-size: 0.82rem;
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    margin-bottom: 0.5rem;
  }
  .access-pick {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.4rem;
    margin-bottom: 0.55rem;
  }
  .access-opt {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    text-align: left;
    border: 1px solid var(--line);
    background: var(--surface-2);
    color: var(--ink);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.5rem;
    font-size: 0.74rem;
  }
  .access-opt span {
    color: var(--ink-faint);
    font-size: 0.66rem;
    line-height: 1.35;
  }
  .access-opt.sel {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .cands {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    max-height: 9rem;
    overflow-y: auto;
    margin-bottom: 0.6rem;
  }
  .cand {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.8rem;
    padding: 0.25rem 0.3rem;
    border-radius: var(--r-sm);
    cursor: pointer;
  }
  .cand:hover {
    background: var(--surface-2);
  }
  .cand-name {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cand-machine {
    color: var(--ink-faint);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--line-strong);
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--ok);
  }
  .wide {
    width: 100%;
  }
  .rooms {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    max-height: 13rem;
    overflow-y: auto;
  }
  .rooms li {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.45rem 0.4rem 0.45rem 0.55rem;
  }
  .rooms li.open {
    background: var(--accent-soft);
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  .r-main {
    flex: 1;
    min-width: 0;
  }
  .r-name {
    font-size: 0.85rem;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }
  .r-open {
    font-size: 0.72rem;
  }
  .in-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
    box-shadow: 0 0 0 3px oklch(0.8 0.17 150 / 0.16);
  }
  .unread {
    background: var(--accent);
    color: #fff;
    font-size: 0.62rem;
    font-weight: 700;
    border-radius: var(--r-pill);
    padding: 0.05rem 0.36rem;
    line-height: 1.2;
  }
  .knock-badge {
    background: var(--warn-soft);
    color: var(--warn);
    border: 1px solid oklch(0.79 0.14 75 / 0.4);
    font-size: 0.6rem;
    font-weight: 700;
    border-radius: var(--r-pill);
    padding: 0.03rem 0.36rem;
    line-height: 1.3;
    white-space: nowrap;
  }
  .r-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .x {
    padding: 0.25rem 0.4rem;
    color: var(--ink-faint);
  }
  .x:hover {
    color: var(--danger);
  }
</style>
