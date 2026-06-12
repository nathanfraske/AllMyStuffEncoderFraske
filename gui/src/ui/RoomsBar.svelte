<script lang="ts">
  // Virtual rooms — the zoom-like calls between your machines (and the
  // people you share with). This bar lists the rooms this device knows
  // and is where new ones are made; joining one opens the call panel
  // (RoomPanel), where everything — mic, camera, screen — starts off.
  import { app } from "../store.svelte";
  import { displayName } from "../types";

  let draftName = $state("");
  let draftMembers = $state<string[]>([]);

  function toggleMember(id: string) {
    draftMembers = draftMembers.includes(id)
      ? draftMembers.filter((m) => m !== id)
      : [...draftMembers, id];
  }

  function create() {
    app.createRoom(draftName, draftMembers);
    draftName = "";
    draftMembers = [];
  }

  function openDraft() {
    // Pre-fill the default so the maker sees what they'll get: a room
    // named after the fleet's owner ("Casey's room"). Still editable.
    draftName = app.defaultRoomName();
    draftMembers = [];
    app.roomDraftOpen = true;
  }

  function memberSummary(ids: string[]): string {
    const names = ids
      .filter((id) => !app.isMe(id))
      .map((id) => app.machineByAnyId(id)?.label ?? "")
      .filter(Boolean);
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
    {#if app.roomDraftOpen}
      <button class="btn small" onclick={() => (app.roomDraftOpen = false)}>Close</button>
    {:else}
      <button class="btn small primary" onclick={openDraft}>+ New room</button>
    {/if}
  </div>

  {#if app.roomDraftOpen}
    <!-- Making a room: name it, pick who's in it. -->
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
      <div class="cands">
        {#each app.roomCandidateNodes as n (n.id)}
          <label class="cand">
            <input
              type="checkbox"
              checked={draftMembers.includes(n.id)}
              onchange={() => toggleMember(n.id)}
            />
            <span class="cand-name">{displayName(n)}</span>
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
        {@const unread = app.roomUnread[r.id] ?? 0}
        {@const present = presentCount(r.id)}
        <li class:open={app.roomOpenId === r.id}>
          <div class="r-main">
            <div class="r-name">
              🪩 {r.name}
              {#if app.isJoined(r.id)}<span class="in-dot" title="You're in this room"></span>{/if}
              {#if unread > 0}<span class="unread" title="Unread chat">{unread}</span>{/if}
            </div>
            <div class="r-sub">
              {app.isRoomHost(r) ? "hosted by you" : `hosted by ${app.roomHostLabel(r)}`}
              · {memberSummary(r.members)}{present > 0 ? ` · ${present} in now` : ""}
            </div>
          </div>
          {#if app.isJoined(r.id)}
            {#if app.roomOpenId !== r.id}
              <button class="btn small primary" onclick={() => app.joinRoom(r.id)}>Open</button>
            {/if}
            <button class="btn small" onclick={() => app.leaveRoom(r.id)}>Leave</button>
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
    width: 20rem;
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
