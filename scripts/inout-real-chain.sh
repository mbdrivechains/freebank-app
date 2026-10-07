#!/bin/bash
# inout-real-chain.sh <work folder>: In and out (v0.3.0) on a regtest chain. freebankd's standing stack (one node,
# FreeBank's treasury opened by a 40-coin deposit) in <work folder>; then the app's ignored real-chain tests for
# Deposit and Withdraw at par against it; then the stack stops.
# FB_IO_NODES: freebankd nodes (default 1). Needs FB_NODE_REPO (the freebank node's checkout, for its standing stack), FB_ROOT (released freebankd binaries,
# e.g. v0.2.19's) and grpcurl (FB_GRPCURL, default ~/.local/bin/grpcurl). CARGO_TARGET_DIR is passed through.
set -euo pipefail
: "${FB_NODE_REPO:?set FB_NODE_REPO to the freebank node checkout}" "${FB_ROOT:?set FB_ROOT to released freebankd binaries}"
W=$(mkdir -p "${1:?usage: inout-real-chain.sh <work folder>}" && cd "$1" && pwd)
APP=$(cd "$(dirname "$0")/.." && pwd)
G=${FB_GRPCURL:-$HOME/.local/bin/grpcurl}
cd "$W"
rm -rf fb-gate.*
(TMPDIR=$W FB_ROOT=$FB_ROOT setsid "$FB_NODE_REPO/test/integration/lib/standing_stack.sh" --nodes "${FB_IO_NODES:-1}" --deposit 40 > stack.log 2>&1 & echo $! > stack.pid)
stop() { kill -- -"$(cat "$W/stack.pid")" 2>/dev/null || true; }
[ "${FB_IO_KEEP:-0}" = 1 ] || trap stop EXIT
for i in $(seq 1 300); do ls fb-gate.*/stack.env >/dev/null 2>&1 && break; sleep 2; done
S=$(ls -d "$W"/fb-gate.*)
cat > env.sh <<ENV
source $S/stack.env
G=$G
FB=\$FB_CLI
enf() { timeout 30 \$G -plaintext -d "\${2:-{\\}}" \$ENFORCER_GRPC "cusf.mainchain.v1.\$1"; }
MA=\$(enf WalletService/CreateNewAddress | sed -n 's/.*"address": *"\([^"]*\)".*/\1/p')
l1() { enf MiningService/GenerateToAddress "{\"blocks\":\$1,\"address\":\"\$MA\"}" >/dev/null; }
bmm() { local t=\$(( \$(\$FB getblockcount) + \${1:-1} )) n=0; while [ "\$(\$FB getblockcount)" -lt "\$t" ]; do \$FB refreshbmm 0.001 >/dev/null 2>&1; l1 1; sleep 0.3; n=\$((n+1)); [ \$n -gt 100 ] && { echo "bmm stuck"; return 1; }; done; \$FB refreshbmm 0.001 >/dev/null 2>&1; true; }
ENV
printf '#!/bin/bash\nsource %s/env.sh >/dev/null 2>&1\nbmm 1\n' "$W" > bmm1.sh
chmod +x bmm1.sh
source ./env.sh
for i in $(seq 1 60); do [ "$($FB getbalance 2>/dev/null | cut -d. -f1)" -ge 39 ] 2>/dev/null && break; bmm 1; sleep 1; done
echo "stack up: FreeBank $($FB getblockcount), balance $($FB getbalance)"
cd "$APP/src-tauri"
FB_IO_L1=$L1_RPC FB_IO_FB=$FB_RPC FB_IO_ENFORCER=$ENFORCER_GRPC FB_IO_BMM=$W/bmm1.sh FB_GRPCURL=$G \
  cargo test -j2 --lib -- --ignored --nocapture --test-threads 1 ${FB_IO_TESTS:-deposit_real_stack withdraw_real_stack} 2>&1 | tail -40
