<script lang="ts">
  // A QR code as inline SVG, dark on white with the standard 4-module quiet zone, with FreeBank's ☉
  // ghosted in gold under it (qr.ts `qrGhost`).
  import { qrGhost, qrMatrix, qrPath } from "../lib/qr";

  export let text: string;
  export let size = 232;

  $: m = qrMatrix(text, "M");
  $: n = m.length + 8;
  $: d = qrPath(m, 4);
  $: ghost = qrGhost(m, 4);
</script>

<svg class="qr" width={size} height={size} viewBox="0 0 {n} {n}" shape-rendering="crispEdges" role="img" aria-label="QR code">
  <rect width={n} height={n} fill="#fff" />
  {#each ghost as g}<path d={g.d} fill={g.fill} />{/each}
  <path {d} fill="#000" />
</svg>

<style>
  .qr {
    display: block;
    border-radius: 8px;
    max-width: 100%;
    height: auto;
  }
</style>
