#!/bin/bash
# changer-notes-real-chain.sh <work folder>: the money changer's notes (v0.4.8, CASH_OUT_DESIGN.md §4.13) end to end
# on a regtest chain. As changer-real-chain.sh, plus a house "nbank" (house 1) registered, attested and minting notes
# on the first node, and the changer dealing in its notes (stock target 0.1 sECX); then the app's ignored real-chain
# test changer_notes_real_stack: sell notes for ECX, buy notes with ECX, and the reflux paid by the house.
# Needs FB_NODE_REPO, FB_ROOT (as inout-real-chain.sh) and FB_DIST (the distribution checkout, for the bot).
set -euo pipefail
: "${FB_DIST:?set FB_DIST to the distribution checkout}"
W=$(mkdir -p "${1:?usage: changer-real-chain.sh <work folder>}" && cd "$1" && pwd)
APP=$(cd "$(dirname "$0")/.." && pwd)
FB_IO_NODES=2 FB_IO_KEEP=1 FB_IO_TESTS=none_at_all "$APP/scripts/inout-real-chain.sh" "$W" | grep -E 'stack up' || true
stop() {
  [ -f "$W/changer.pid" ] && kill "$(cat "$W/changer.pid")" 2>/dev/null || true
  kill -- -"$(cat "$W/stack.pid")" 2>/dev/null || true
}
trap stop EXIT
source "$W/env.sh"
umask 077
mkdir -p "$W/changer"
C=$W/changer
echo t > "$C/rpc.pass"
head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$C/ecash.passphrase"
# The bot's eCash wallet: encrypted, funded by mining (100 blocks to mature).
$L1_CLI createwallet changer false false "$(cat "$C/ecash.passphrase")" >/dev/null
CA=$($L1_CLI -rpcwallet=changer getnewaddress "" bech32)
enf MiningService/GenerateToAddress "{\"blocks\":110,\"address\":\"$CA\"}" >/dev/null
# The bot's FreeBank wallet: the stack's second node's, funded from the first's.
FA=$($N2_CLI getnewaddress "" legacy)
$FB sendtoaddress "$FA" 20 >/dev/null
bmm 2
for i in $(seq 1 30); do [ "$($N2_CLI getbalance | cut -d. -f1)" -ge 20 ] 2>/dev/null && break; bmm 1; done
echo "changer funds: eCash $($L1_CLI -rpcwallet=changer getbalance), FreeBank $($N2_CLI getbalance)"
# The house: registered, attested a block later (registration counts as its first attestation), its notes minted to
# the first node's wallet (the user's, in the test).
$FB registerhouse 0 1 nbank 1000 "[1.0]" 0.001 >/dev/null
bmm 2
$FB attesthouse 1 0.001 >/dev/null
bmm 2
$FB mintnote 1 50000000 0.001 >/dev/null
bmm 2
echo "house 1: $($FB gethouse 1 | tr -d ' \n' | grep -oE '"(classid|effective_status|mintedunits)":[^,]*' | tr '\n' ' ')"
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
cat > "$C/config.json" <<CFG
{
  "listen": "127.0.0.1:$PORT",
  "key_file": "$C/changer.key",
  "book_file": "$C/orders.json",
  "every": 2,
  "ecash": {"url": "http://$L1_RPC", "user": "t", "password_file": "$C/rpc.pass", "wallet": "changer", "passphrase_file": "$C/ecash.passphrase"},
  "freebank": {"url": "http://$N2_RPC", "user": "t", "password_file": "$C/rpc.pass"},
  "notes": {"1": {"max_units": 100000000, "target_units": 10000000}}
}
CFG
( cd "$FB_DIST/changer" && cargo build -j2 --quiet )
BIN=${CARGO_TARGET_DIR:-$FB_DIST/changer/target}/debug/fb-changer
KEY=$("$BIN" --config "$C/config.json" --new-key)
(RUST_LOG=fb_changer=info "$BIN" --config "$C/config.json" > "$C/changer.log" 2>&1 & echo $! > "$W/changer.pid")
for i in $(seq 1 50); do curl -sf --max-time 5 "http://127.0.0.1:$PORT/v1/info" >/dev/null && break; sleep 0.2; done
echo "changer up on $PORT, key $KEY"
# FB_NOTES_HOLD=1: for a walk through the app, no test. Write what the app needs to walk.env and make a FreeBank block
# every 15 seconds until stopped.
if [ "${FB_NOTES_HOLD:-0}" = 1 ]; then
  printf 'PORT=%s\nKEY=%s\n' "$PORT" "$KEY" > "$W/walk.env"
  cat "$W/fb-gate."*/stack.env >> "$W/walk.env"
  echo "holding: $W/walk.env"
  while :; do bmm 1 >/dev/null 2>&1 || true; sleep 15; done
fi
cd "$APP/src-tauri"
FB_IO_L1=$L1_RPC FB_IO_FB=$FB_RPC FB_IO_ENFORCER=$ENFORCER_GRPC FB_IO_BMM=$W/bmm1.sh FB_GRPCURL=$G \
FB_CHANGER_URL=http://127.0.0.1:$PORT FB_CHANGER_KEY=$KEY \
  cargo test -j2 --lib -- --ignored --nocapture --test-threads 1 changer_notes_real_stack 2>&1 | tail -30
echo "--- changer log (notes, reflux)"; grep -E "reflux|order|warn" "$C/changer.log" | tail -25 || true
