<script lang="ts">
  // Settings › Node & connection: the eCash node the eCash tab uses (v0.2.6; the UX walk-through found its error naming
  // a setting no screen showed). Empty fields mean the defaults: the address Setup found, and the login looked for as
  // BitWindow does (the node's cookie or bitcoin.conf, then BitWindow's). A node on another computer needs its user and
  // password: the password is kept in a file only this user can read.
  import { onMount } from "svelte";
  import { ecashLoginGet, ecashLoginSet, type EcashLogin } from "../lib/ecash";

  let login: EcashLogin | null = null;
  let rpc = "";
  let datadir = "";
  let user = "";
  let password = "";
  let busy = false;
  let result = "";
  let ok = false;

  onMount(async () => {
    try {
      login = await ecashLoginGet();
      rpc = login.l1_rpc;
      datadir = login.l1_datadir;
      user = login.user;
    } catch (e) {
      result = String(e);
    }
  });

  async function save() {
    busy = true;
    result = "";
    try {
      // An empty password field keeps the saved one; clearing the user forgets it.
      const pw = !user.trim() ? "" : password ? password : null;
      const st = await ecashLoginSet(rpc, datadir, user, pw);
      password = "";
      login = await ecashLoginGet();
      ok = !st.problem;
      result = st.problem ?? (st.state === "ready" ? "Connected; your eCash wallets are there." : "Connected.");
    } catch (e) {
      ok = false;
      result = String(e).replace(/^Error: /, "");
    }
    busy = false;
  }
</script>

<div class="card" data-testid="ecash-login">
  <h3>eCash node for the eCash tab</h3>
  <p class="muted small">
    Leave these empty for the eCash node Setup found{login ? ` (${login.rpc})` : ""}, logged into as BitWindow does.
  </p>
  <p class="muted small" data-testid="ecash-login-remote">
    For a node on another computer: its address, and a user and password it accepts: the <code>rpcuser=</code> and
    <code>rpcpassword=</code> lines of its config file (its <code>bitcoin.conf</code>, or the file named by its
    <code>-conf</code>). A node that uses <code>rpcauth=</code> or only a cookie needs a user and password added. That
    node must also let this computer in (<code>rpcbind=</code> and <code>rpcallowip=</code>). Leave the data folder
    empty.
  </p>
  <form class="form" on:submit|preventDefault={save}>
    <label>
      Address (host:port)
      <input type="text" bind:value={rpc} placeholder={login?.rpc ?? "127.0.0.1:18302"} spellcheck="false" autocomplete="off" />
    </label>
    <label>
      Its data folder, if it runs on this computer
      <input type="text" bind:value={datadir} placeholder="e.g. ~/.ecash" spellcheck="false" autocomplete="off" />
    </label>
    <label>
      User
      <input type="text" bind:value={user} placeholder="its rpcuser" spellcheck="false" autocomplete="off" />
    </label>
    <label>
      Password
      <input type="password" bind:value={password} placeholder={login?.has_password ? "saved" : ""} autocomplete="off" />
    </label>
    <button type="submit" disabled={busy}>{busy ? "Checking…" : "Save and check"}</button>
  </form>
  {#if result}<p class={ok ? "hint ok-note" : "soft-error"}>{result}</p>{/if}
</div>
