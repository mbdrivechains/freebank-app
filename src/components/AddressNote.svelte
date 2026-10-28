<script lang="ts">
  // Under an address Receive just handed out (v0.6.8): a note of what it is for, kept with it in Settings › Wallet's list
  // of addresses given out. Only this computer has it.
  import { given, NOTE_MAX } from "../lib/given";

  export let address: string;
  export let kind: "freebank" | "ecash";

  let note = "";
  let saved = "";
  let error = "";
  let saving = false;
  let lastAddress = "";
  // A new address starts with an empty note.
  $: if (address !== lastAddress) {
    lastAddress = address;
    note = "";
    saved = "";
    error = "";
  }

  async function save() {
    saving = true;
    error = "";
    try {
      await given.note(address, kind, note);
      saved = note.trim();
    } catch (e) {
      error = String(e);
    }
    saving = false;
  }
</script>

<form class="address-note" on:submit|preventDefault={save} data-testid="address-note">
  <input
    type="text"
    bind:value={note}
    maxlength={NOTE_MAX}
    placeholder="Note: what is it for?"
    on:input={() => (saved = "")}
    data-testid="address-note-input"
  />
  <button class="secondary" type="submit" disabled={saving || note.trim() === saved}>{saved && note.trim() === saved ? "Saved" : "Save"}</button>
</form>
{#if error}<p class="soft-error">{error}</p>{/if}

<style>
  .address-note {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .address-note input {
    flex: 1;
    min-width: 0;
  }
</style>
