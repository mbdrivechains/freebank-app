#!/usr/bin/env bash
# The phone remote end to end against a real FreeBank node, everything on 127.0.0.1:
#   - fb-relay, built from freebank-distribution/relay, serving a fresh build of its phone page;
#   - this app's desktop side (the page_host test) using the node's wallet over RPC;
#   - the real page in headless WebKit, Chromium and Firefox (scripts/phone-real-node.mjs).
# The node must be running and synced; this script never starts or stops it. The relay and page must
# be at least freebank-distribution b0f26ea (the comparison code, P1, and the proof's context, P4).
#
#   FB_NODE_DATADIR=<datadir> FB_NODE_RPCPORT=<port> FB_CLI=<freebank-cli> \
#     [FB_PASS_FILE=<0600 file>] scripts/phone-real-node.sh [webkit] [chromium] [firefox]
#
# Env:
#   FB_NODE_DATADIR  the node's datadir; its .cookie is read there (required)
#   FB_NODE_RPCPORT  the node's RPC port on 127.0.0.1 (required)
#   FB_CLI           freebank-cli (default: freebank-cli on PATH)
#   FB_PASS_FILE     a 0600 file with the wallet passphrase (required if the wallet is encrypted;
#                    never printed)
#   FB_DIST          the freebank-distribution checkout (default: found beside this repo)
#   FB_WORK          builds, the app folder, logs and screenshots (default: a new folder in /tmp)
#   FB_HELD_SECS     held sends expire after this many seconds in the run (default 20)
#   FB_SEND_ECX      the send within the daily limit (default 0.01; the limit is 0.1)
#   FB_OVER_ECX      the send over the limit, confirmed on the desktop (default 0.5)
#   FB_TO            where the sends pay (default: a fresh address of the node's own wallet)
# With coins in the wallet the sends go out (to the wallet itself unless FB_TO says otherwise, so
# only fees are spent) and each receipt's txid is checked with the node; with an empty wallet
# each must fail with "Not enough ECX in your desktop wallet for this payment and its fee."
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"

die() { echo "phone-real-node: $*" >&2; exit 1; }
: "${FB_NODE_DATADIR:?set FB_NODE_DATADIR to the node's datadir}"
: "${FB_NODE_RPCPORT:?set FB_NODE_RPCPORT to the node's RPC port}"
FB_CLI="${FB_CLI:-$(command -v freebank-cli || true)}"
[ -x "$FB_CLI" ] || die "set FB_CLI to freebank-cli"
[ -r "$FB_NODE_DATADIR/.cookie" ] || die "no readable .cookie in $FB_NODE_DATADIR (is the node running with cookie auth?)"
if [ -z "${FB_DIST:-}" ]; then
  for d in "$REPO/../distribution" "$REPO/../../distribution"; do
    [ -f "$d/relay/Cargo.toml" ] && FB_DIST="$(cd "$d" && pwd)" && break
  done
fi
[ -f "${FB_DIST:-}/relay/Cargo.toml" ] || die "set FB_DIST to the freebank-distribution checkout"
PHONE="$FB_DIST/phone"
[ -d "$PHONE/node_modules/playwright" ] || die "no Playwright in $PHONE/node_modules (npm install; npm run e2e:setup there)"
FB_WORK="${FB_WORK:-$(mktemp -d /tmp/fb-phone-real-node.XXXXXX)}"
mkdir -p "$FB_WORK"
export CARGO_BUILD_JOBS=2

cli() { "$FB_CLI" -datadir="$FB_NODE_DATADIR" -rpcport="$FB_NODE_RPCPORT" "$@"; }
cli getblockcount >/dev/null || die "the node doesn't answer on 127.0.0.1:$FB_NODE_RPCPORT"
FB_ENCRYPTED=0
if cli getwalletinfo | grep -q '"unlocked_until"'; then
  FB_ENCRYPTED=1
  [ -r "${FB_PASS_FILE:-}" ] || die "the wallet is encrypted: set FB_PASS_FILE to a file with its passphrase"
  perm="$(stat -c %a "$FB_PASS_FILE")"
  [ "$perm" = 600 ] || [ "$perm" = 400 ] || die "$FB_PASS_FILE must be mode 0600 (it is $perm)"
fi
echo "node: height $(cli getblockcount), wallet $([ $FB_ENCRYPTED = 1 ] && echo encrypted || echo 'not encrypted'); work folder $FB_WORK"

echo "== building fb-relay, the phone page and the desktop side (2 jobs)"
cargo build -j 2 --quiet --manifest-path "$FB_DIST/relay/Cargo.toml" --target-dir "$FB_WORK/relay-target"
# Vite writes a temporary copy of its config next to the config file, so the page is built through
# a wrapper config here: nothing is written into the distribution tree.
cat > "$FB_WORK/vite.phone.config.mjs" <<EOF
import base from '$PHONE/vite.config.ts';
export default { ...base, root: '$PHONE', build: { ...base.build, outDir: '$FB_WORK/phone-dist', emptyOutDir: true } };
EOF
(cd "$FB_WORK" && node "$PHONE/node_modules/vite/bin/vite.js" build --logLevel warn --config "$FB_WORK/vite.phone.config.mjs")
TESTBIN="$(cd "$REPO/src-tauri" && cargo test -j 2 --no-run 2>&1 | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p')"
[ -n "$TESTBIN" ] || die "couldn't build the desktop side's tests"
TESTBIN="$REPO/src-tauri/$TESTBIN"

free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
PORT="$(free_port)"
CTL="$FB_WORK/ctl"
APP="$FB_WORK/app-$(date +%s)"
rm -f "$CTL/pair-url" "$CTL/ack" "$CTL/cmd"
mkdir -p "$CTL" "$APP"

RELAY_PID=""
HOST_PID=""
cleanup() {
  # Only what this script started, by PID.
  if [ -n "$HOST_PID" ] && kill -0 "$HOST_PID" 2>/dev/null; then
    printf 'q quit\n' > "$CTL/.cmd.tmp" && mv "$CTL/.cmd.tmp" "$CTL/cmd"
    for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$HOST_PID" 2>/dev/null || break; sleep 0.5; done
    kill "$HOST_PID" 2>/dev/null || true
  fi
  [ -n "$RELAY_PID" ] && kill "$RELAY_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "== fb-relay on 127.0.0.1:$PORT"
# Only the page's own origin may open /ws from a browser, as on app.ecxfreebank.com.
"$FB_WORK/relay-target/debug/fb-relay" --listen "127.0.0.1:$PORT" --static "$FB_WORK/phone-dist" \
  --origin "http://127.0.0.1:$PORT" > "$FB_WORK/relay.log" 2>&1 &
RELAY_PID=$!
for i in $(seq 1 100); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/" && break
  kill -0 "$RELAY_PID" 2>/dev/null || die "fb-relay stopped; see $FB_WORK/relay.log"
  sleep 0.1
done

echo "== the desktop side (app folder $APP)"
FB_RELAY_URL="ws://127.0.0.1:$PORT/ws" FB_CTL_DIR="$CTL" FREEBANK_APP_DIR="$APP" \
  FB_NODE_URL="http://127.0.0.1:$FB_NODE_RPCPORT" FB_NODE_COOKIE="$FB_NODE_DATADIR/.cookie" \
  FB_PASS_FILE="${FB_PASS_FILE:-}" FB_HELD_SECS="${FB_HELD_SECS:-20}" FB_AUTO_ALLOW=0 FB_AUTO_CONFIRM=0 FB_HOST_SECS=3600 \
  "$TESTBIN" phone::tests::page_host --exact --ignored --nocapture > "$FB_WORK/host.log" 2>&1 &
HOST_PID=$!
for i in $(seq 1 300); do
  [ -f "$CTL/pair-url" ] && break
  kill -0 "$HOST_PID" 2>/dev/null || die "the desktop side stopped; see $FB_WORK/host.log"
  sleep 0.1
done
[ -f "$CTL/pair-url" ] || die "the desktop side didn't come online; see $FB_WORK/host.log"

echo "== the phone page in the browsers"
set +e
FB_PHONE_DIR="$PHONE" FB_ORIGIN="http://127.0.0.1:$PORT" FB_CTL_DIR="$CTL" FB_SHOTS="$FB_WORK/shots" \
  FB_CLI="$FB_CLI" FB_NODE_DATADIR="$FB_NODE_DATADIR" FB_NODE_RPCPORT="$FB_NODE_RPCPORT" \
  FB_HELD_SECS="${FB_HELD_SECS:-20}" FB_SEND_ECX="${FB_SEND_ECX:-0.01}" FB_OVER_ECX="${FB_OVER_ECX:-0.5}" \
  FB_TO="${FB_TO:-}" FB_ENCRYPTED="$FB_ENCRYPTED" \
  node "$REPO/scripts/phone-real-node.mjs" "$@"
status=$?
set -e
echo "screenshots: $FB_WORK/shots; desktop log: $FB_WORK/host.log; relay log: $FB_WORK/relay.log"
exit $status
