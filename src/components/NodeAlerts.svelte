<script lang="ts">
  // Update alerts (v0.7.6, lib/alerts.ts): a node the app uses has a warning, or has stopped adding blocks. On every
  // screen while it holds, with what to do; no dismiss, since a node left behind by a fork stays behind.
  import { createEventDispatcher } from "svelte";
  import { nodeAlerts, type NodeAlert } from "../lib/alerts";
  import { appUpdate } from "../lib/appUpdate";
  import { network } from "../lib/network";
  import { checkForUpdate, openUrl, quietFor, update, when } from "../lib/node";

  const dispatch = createEventDispatcher<{ open: "node" | "about" }>();
  const RELEASES = "https://github.com/mbdrivechains/freebank-app/releases";

  function whose(a: NodeAlert): string {
    return a.node === "freebank" ? "FreeBank's node" : `Your ${$network.l1} node`;
  }
  function title(a: NodeAlert): string {
    if (a.kind === "quiet") return `${whose(a)} has had no new block for ${quietFor(a.hours ?? 2)}`;
    return a.kind === "stalled" ? `${whose(a)} has stopped adding blocks` : `${whose(a)} has a warning`;
  }
  function peers(n: number): string {
    return `${n} peer${n === 1 ? "" : "s"}`;
  }

  // v0.7.10: read the update offer again when an alert first shows, so the box never says "No update is offered yet"
  // from a stale read (the screens' own check can skip while the window is hidden).
  let shown = false;
  $: if ($nodeAlerts.length && !shown) {
    shown = true;
    checkForUpdate();
  } else if (!$nodeAlerts.length) {
    shown = false;
  }

  /** The FreeBank node is on a release the network's chain no longer runs on (easynet's new chains). */
  function leftBehind(a: NodeAlert): boolean {
    return a.node === "freebank" && !!$update?.needs;
  }
</script>

{#if $nodeAlerts.length}
  <div class="card node-alerts" role="alert" data-testid="node-alerts">
    {#each $nodeAlerts as a}
      <p class="alert-title"><strong>{title(a)}</strong></p>
      {#if (a.kind === "quiet" || a.kind === "stalled") && leftBehind(a)}
        <p data-testid="node-left-behind">
          FreeBank on {$network.name} has moved to a new chain, which needs FreeBank {$update?.needs} or newer. This node runs
          {$update?.installed}, so it stayed on the old chain, where no new blocks come. Updating the node moves it to the
          new chain. Your wallets keep their keys and addresses, but coins and payments on the old chain don't carry over.
        </p>
      {:else if a.kind === "quiet"}
        <p data-testid="node-quiet">
          Its newest block, {(a.blocks ?? 0).toLocaleString()}, is from {a.tip_time ? when(a.tip_time) : "hours ago"}.
          {#if !a.peers}
            It has no peers to hear of newer ones from: check that this computer is online. If it is, the network may
            have moved on to a release this node isn't running.
          {:else}
            It has {peers(a.peers)}, and none has a newer block: the network may have paused.
          {/if}
        </p>
      {:else if a.kind === "stalled"}
        <p>
          It has block {(a.blocks ?? 0).toLocaleString()} and knows of blocks up to {(a.headers ?? 0).toLocaleString()},
          and it hasn't moved on for 30 minutes. A node that rejects the blocks the rest of the network accepts stops like
          this; it may need an update.
        </p>
      {:else}
        <p class="said">“{a.text}”</p>
      {/if}
    {/each}
    {#if $update?.available}
      <p>
        A newer FreeBank node is ready ({$update.latest}).
        <button class="link" on:click={() => dispatch("open", "node")}>Update the FreeBank node on the Node tab</button>
      </p>
    {:else if $appUpdate?.available}
      <p>
        FreeBank {$appUpdate.latest} is out, and you have {$appUpdate.current}.
        <button class="link" on:click={() => dispatch("open", "about")}>Update it in Settings › About</button>
      </p>
    {:else}
      <p class="muted">
        No update is offered yet. When one is needed, the app offers it here, or it comes as a new FreeBank app, which
        updates the nodes it installs:
        <button class="link" on:click={() => openUrl(RELEASES)}>FreeBank's releases</button>. A node you run yourself
        updates from its own releases.
      </p>
    {/if}
  </div>
{/if}

<style>
  .node-alerts {
    border-color: var(--error-color);
  }
  .node-alerts p {
    margin: 0 0 8px;
  }
  .node-alerts p:last-child {
    margin-bottom: 0;
  }
  .alert-title {
    color: var(--error-color);
  }
  .said {
    overflow-wrap: anywhere;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent-color);
    text-decoration: underline;
    cursor: pointer;
    font: inherit;
  }
</style>
