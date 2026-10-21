<script lang="ts">
  // Home (v0.6.1): the app's own eCash node or enforcer stopped on its own (the watch, stack.rs `check_gone`). It says
  // when, what its log said, and whether FreeBank started it again; Start them while one is still down. Looked at every
  // 30 seconds; × hides that one stop for the session (stoppedOwnHidden: this component goes with Home).
  import { onDestroy, onMount } from "svelte";
  import { network, stack, stoppedOwnHidden, stoppedOwnWords, type StackStatus } from "../lib/network";
  import Notice from "./Notice.svelte";

  let own: StackStatus | null = null;
  let busy = false;
  let error = "";
  let timer: ReturnType<typeof setInterval> | undefined;

  async function load() {
    if (!$network.stack) return;
    own = await stack.status().catch(() => own);
  }
  onMount(() => {
    load();
    timer = setInterval(load, 30000);
  });
  onDestroy(() => clearInterval(timer));

  async function start() {
    busy = true;
    error = "";
    try {
      await stack.start();
    } catch (e) {
      error = String(e);
    }
    await load();
    busy = false;
  }

  $: gone = own?.stopped_own ?? null;
  $: down = !!own && !(own.node_up && own.enforcer_up);
</script>

{#if gone && gone.at !== $stoppedOwnHidden}
  <div data-testid="stack-alert">
    <Notice kind={down ? "error" : "info"} message={stoppedOwnWords(gone)} on:dismiss={() => stoppedOwnHidden.set(gone?.at ?? 0)}>
      {stoppedOwnWords(gone)}
      {#if down}
        <span class="row-actions">
          <button class="secondary" on:click={start} disabled={busy}>{busy ? "Starting…" : "Start them"}</button>
        </span>
      {/if}
      {#if error}<span class="soft-error">{error}</span>{/if}
    </Notice>
  </div>
{/if}
