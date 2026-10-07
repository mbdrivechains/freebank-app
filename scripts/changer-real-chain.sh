#!/bin/bash
# changer-real-chain.sh <work folder>: the money changer end to end on a regtest chain. freebankd's standing stack
# (inout-real-chain.sh with two FreeBank nodes, kept up), then the changer bot (distribution/changer) with wallets of
# its own: an encrypted eCash wallet funded by mining, the second node's FreeBank wallet funded from the first's; then the app's ignored real-chain test
# changer_real_stack against it; then everything stops.
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
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
cat > "$C/config.json" <<CFG
{
  "listen": "127.0.0.1:$PORT",
  "key_file": "$C/changer.key",
  "book_file": "$C/orders.json",
  "every": 2,
  "ecash": {"url": "http://$L1_RPC", "user": "t", "password_file": "$C/rpc.pass", "wallet": "changer", "passphrase_file": "$C/ecash.passphrase"},
  "freebank": {"url": "http://$N2_RPC", "user": "t", "password_file": "$C/rpc.pass"}
}
CFG
( cd "$FB_DIST/changer" && cargo build -j2 --quiet )
BIN=${CARGO_TARGET_DIR:-$FB_DIST/changer/target}/debug/fb-changer
KEY=$("$BIN" --config "$C/config.json" --new-key)
("$BIN" --config "$C/config.json" > "$C/changer.log" 2>&1 & echo $! > "$W/changer.pid")
for i in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/v1/info" >/dev/null && break; sleep 0.2; done
echo "changer up on $PORT, key $KEY"
cd "$APP/src-tauri"
FB_IO_L1=$L1_RPC FB_IO_FB=$FB_RPC FB_IO_ENFORCER=$ENFORCER_GRPC FB_IO_BMM=$W/bmm1.sh FB_GRPCURL=$G \
FB_CHANGER_URL=http://127.0.0.1:$PORT FB_CHANGER_KEY=$KEY \
  cargo test -j2 --lib -- --ignored --nocapture --test-threads 1 changer_real_stack 2>&1 | tail -30
echo "--- changer log (tail)"; tail -15 "$C/changer.log"
