<script lang="ts">
  // What's running where (v0.2.5 item 14): your phones, the relay, this computer (the app, the phone link, the FreeBank node), the eCash
  // node and enforcer, and the FreeBank network. Each with a dot, and what happens to it when you quit.
  import { onDestroy, onMount } from "svelte";
  import type { NodeStatus } from "../lib/node";
  import { phone, type KeepInfo, type PhoneDevice, type RelayStatus } from "../lib/phone";

  export let st: NodeStatus;
  /** The node was started by another program. */
  export let external = false;

  let relay: RelayStatus | null = null;
  let devices: PhoneDevice[] = [];
  let keep: KeepInfo | null = null;
  async function load() {
    try {
      [relay, devices, keep] = await Promise.all([phone.status(), phone.devices(), phone.keepInfo()]);
    } catch {
      // The phone link isn't set up on this computer: the map shows no phones.
    }
  }
  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    load();
    timer = setInterval(load, 15_000);
  });
  onDestroy(() => clearInterval(timer));

  function host(addr: string): string {
    try {
      return new URL(addr.includes("://") ? addr : `http://${addr}`).hostname.replace(/^\[|\]$/g, "");
    } catch {
      return addr;
    }
  }
  const here = (addr: string) => ["127.0.0.1", "localhost", "::1"].includes(host(addr));

  $: online = devices.filter((d) => d.online).length;
  $: relayUp = relay?.state === "online";
  $: nodeWord =
    st.state === "up" ? "running" : st.state === "warming" || st.state === "busy" ? "starting" : st.state === "locked" ? "running" : "stopped";
  $: nodeAfter = external
    ? "another program runs it; FreeBank leaves it alone"
    : st.keeps_running
      ? "keeps running when you quit"
      : "stops when you quit";
  // The background part starts at quit only when the node outlives the app (phone_keep_connected_quit, keep_at_exit).
  $: linkAfter = !devices.length
    ? "off until you pair a phone"
    : keep?.keep && (st.keeps_running || external)
      ? `a small background part keeps it when you quit${keep.at_login ? ", and starts it when you log in" : ""}`
      : "stops when you quit";
  $: l1Up = st.l1_blocks !== null;
</script>

<div class="card map" data-testid="whats-running">
  <h3>What's running where</h3>
  <ol class="chain">
    <li class="part">
      <span class="dot" class:on={online > 0}></span>
      <div>
        <strong>Your phones</strong>
        <span class="muted small">{devices.length ? `${online} connected · ${devices.length} paired` : "none paired"}</span>
      </div>
    </li>
    {#if devices.length}
      <li class="link small muted">
        through the relay at {relay ? host(relay.url) : "…"} · {relayUp ? "connected" : (relay?.state ?? "…")}
      </li>
    {:else}
      <li class="link"></li>
    {/if}
    <li class="part box">
      <strong>This computer</strong>
      <ul>
        <li>
          <span class="dot on"></span>
          <div><span>FreeBank app</span> <span class="muted small">open now</span></div>
        </li>
        <li>
          <span class="dot" class:on={devices.length > 0 && relayUp}></span>
          <div><span>Phone link</span> <span class="muted small">{linkAfter}</span></div>
        </li>
        <li>
          <span class="dot" class:on={nodeWord === "running"} class:warn={nodeWord === "starting"}></span>
          <div><span>FreeBank node</span> <span class="muted small">{nodeWord} · {nodeAfter}</span></div>
        </li>
        {#if !st.demo && here(st.rest)}
          <li>
            <span class="dot" class:on={l1Up}></span>
            <div>
              <span>eCash node and enforcer</span>
              <span class="muted small">{l1Up ? `block ${st.l1_blocks?.toLocaleString()}` : "not answering"} · run by BitWindow or yourself; FreeBank doesn't stop them</span>
            </div>
          </li>
        {/if}
      </ul>
    </li>
    {#if st.demo}
      <li class="link"></li>
      <li class="part">
        <span class="dot" class:on={l1Up}></span>
        <div>
          <strong>FreeBank's gateway</strong>
          <span class="muted small">demo mode: eCash facts from {st.enforcer} · {l1Up ? `block ${st.l1_blocks?.toLocaleString()}` : "not answering"}</span>
        </div>
      </li>
    {:else if !here(st.rest)}
      <li class="link"></li>
      <li class="part">
        <span class="dot" class:on={l1Up}></span>
        <div>
          <strong>eCash node and enforcer</strong>
          <span class="muted small">at {host(st.rest)} · {l1Up ? `block ${st.l1_blocks?.toLocaleString()}` : "not answering"}</span>
        </div>
      </li>
    {/if}
    <li class="link"></li>
    <li class="part">
      <span class="dot" class:on={st.peers.length > 0}></span>
      <div>
        <strong>FreeBank network</strong>
        <span class="muted small">{st.peers.length} peer{st.peers.length === 1 ? "" : "s"}</span>
      </div>
    </li>
  </ol>
</div>

<style>
  .map h3 {
    margin: 0 0 10px;
  }
  .chain {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .part {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .part > div,
  .box li > div {
    display: flex;
    flex-direction: column;
  }
  .box {
    flex-direction: column;
    gap: 6px;
    border: 1px solid var(--border-color);
    border-radius: 10px;
    padding: 10px 12px;
  }
  .box ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .box li {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .link {
    margin-left: 4px;
    padding: 4px 0 4px 14px;
    border-left: 2px dotted var(--border-color);
    min-height: 14px;
  }
  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    margin-top: 5px;
    border-radius: 50%;
    background: var(--text-secondary);
    opacity: 0.5;
  }
  .dot.on {
    background: var(--success-color);
    opacity: 1;
  }
  .dot.warn {
    background: var(--accent-color);
    opacity: 1;
  }
</style>
