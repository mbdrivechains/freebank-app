<script lang="ts">
  // A file path that wraps only after a slash, so a long one breaks between folder names rather
  // than in the middle of one (a hyphen inside a name is no place to break). Each name keeps to
  // one line unless it is wider than the whole line on its own.
  export let path: string;
  $: parts = path.split("/").map((p, i, all) => (i < all.length - 1 ? `${p}/` : p));
</script>

{#each parts as part}<span class="path-part">{part}</span><wbr />{/each}
