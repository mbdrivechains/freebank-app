#!/usr/bin/env bash
# Hosted wallets (v0.2.8) end to end, everything on 127.0.0.1: fb-relay serving a fresh build of the phone page, two
# desktops (the page_host test) on two freebankd nodes of one regtest chain, and the real page in headless Chromium
# with a WebAuthn virtual authenticator per phone (scripts/hosted-real-node.mjs): an owner's phone invites a
# shopkeeper's phone into a members-only house, the house's desktop makes his wallet (its node restarts), he joins,
# is paid by Scan to pay, pays back, and moves home: desktop B is set up as new (its own words), and the house moves his
# notes and ECX to the address desktop B gives (PROTOCOL.md, "Moving home (to fresh words)").
#
# The chain is set up by hand first (see the env below): node A's wallet runs a members-only house, holds its notes,
# and is encrypted; node B is a fresh node on the same chain, with an unencrypted new wallet. Blocks come from FB_BMM_CMD.
#
#   FB_A_URL FB_A_COOKIE FB_A_CLI FB_A_PASS_FILE FB_A_RESTART_CMD FB_A_DATADIR  node A (the house's desktop)
#   FB_B_URL FB_B_COOKIE FB_B_CLI FB_B_PASS_FILE FB_B_RESTART_CMD             node B (the shopkeeper's new computer;
#                                                                             its passphrase and restart, for setup-new)
#   FB_BMM_CMD    a shell command that makes one FreeBank block
#   FB_DIST       the freebank-distribution checkout (the page and relay; default: beside this repo)
#   FB_WORK       builds, app folders, logs and screenshots
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
die() { echo "hosted-real-node: $*" >&2; exit 1; }
for v in FB_A_URL FB_A_COOKIE FB_A_CLI FB_A_PASS_FILE FB_A_RESTART_CMD FB_A_DATADIR FB_B_URL FB_B_COOKIE FB_B_CLI FB_B_PASS_FILE FB_B_RESTART_CMD FB_BMM_CMD FB_DIST FB_WORK; do
  [ -n "${!v:-}" ] || die "set $v"
done
PHONE="$FB_DIST/phone"
mkdir -p "$FB_WORK"
export CARGO_BUILD_JOBS=2

echo "== building fb-relay, the phone page and the desktop side (2 jobs)"
cargo build -j 2 --quiet --manifest-path "$FB_DIST/relay/Cargo.toml" --target-dir "$FB_WORK/relay-target"
cat > "$FB_WORK/vite.phone.config.mjs" <<EOF
import base from '$PHONE/vite.config.ts';
export default { ...base, root: '$PHONE', build: { ...base.build, outDir: '$FB_WORK/phone-dist', emptyOutDir: true } };
EOF
(cd "$FB_WORK" && node "$PHONE/node_modules/vite/bin/vite.js" build --logLevel warn --config "$FB_WORK/vite.phone.config.mjs")
TESTBIN="$(cd "$REPO/src-tauri" && cargo test -j 2 --no-run 2>&1 | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p')"
[ -n "$TESTBIN" ] || die "couldn't build the desktop side's tests"
case "$TESTBIN" in /*) ;; *) TESTBIN="$REPO/src-tauri/$TESTBIN" ;; esac

PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
# The page's origin is localhost (a passkey's relying party can't be an IP address); it resolves to 127.0.0.1.
ORIGIN="http://localhost:$PORT"
PIDS=()
cleanup() {
  for c in "$FB_WORK/ctl-a" "$FB_WORK/ctl-b"; do
    [ -d "$c" ] && printf 'q quit\n' > "$c/.cmd.tmp" && mv "$c/.cmd.tmp" "$c/cmd" || true
  done
  sleep 2
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

echo "== fb-relay on 127.0.0.1:$PORT"
"$FB_WORK/relay-target/debug/fb-relay" --listen "127.0.0.1:$PORT" --static "$FB_WORK/phone-dist" --origin "$ORIGIN" \
  > "$FB_WORK/relay.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 100); do curl -sf -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done

host() { # host <a|b> <url> <cookie> [extra env...]
  local n=$1 url=$2 cookie=$3; shift 3
  rm -rf "$FB_WORK/ctl-$n" "$FB_WORK/app-$n"; mkdir -p "$FB_WORK/ctl-$n" "$FB_WORK/app-$n"
  env "$@" FB_RELAY_URL="ws://localhost:$PORT/ws" FB_CTL_DIR="$FB_WORK/ctl-$n" FREEBANK_APP_DIR="$FB_WORK/app-$n" \
    FB_NODE_URL="$url" FB_NODE_COOKIE="$cookie" FB_AUTO_ALLOW=1 FB_AUTO_CONFIRM=0 FB_HOST_SECS=3600 \
    "$TESTBIN" phone::tests::page_host --exact --ignored --nocapture > "$FB_WORK/host-$n.log" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 300); do [ -f "$FB_WORK/ctl-$n/pair-url" ] && return 0; sleep 0.1; done
  die "desktop $n didn't come online; see $FB_WORK/host-$n.log"
}
echo "== desktop A (the house) and desktop B (the new computer)"
host a "$FB_A_URL" "$FB_A_COOKIE" FB_PASS_FILE="$FB_A_PASS_FILE" FB_RESTART_CMD="$FB_A_RESTART_CMD"
host b "$FB_B_URL" "$FB_B_COOKIE" FB_PASS_FILE="$FB_B_PASS_FILE" FB_RESTART_CMD="$FB_B_RESTART_CMD"

echo "== the two phones"
set +e
FB_PHONE_DIR="$PHONE" FB_ORIGIN="$ORIGIN" FB_CTL_A="$FB_WORK/ctl-a" FB_CTL_B="$FB_WORK/ctl-b" FB_SHOTS="$FB_WORK/shots" \
  FB_A_CLI="$FB_A_CLI" FB_A_PASS_FILE="$FB_A_PASS_FILE" FB_A_DATADIR="$FB_A_DATADIR" FB_A_RESTART_CMD="$FB_A_RESTART_CMD" \
  FB_B_CLI="$FB_B_CLI" FB_BMM_CMD="$FB_BMM_CMD" FB_WORK="$FB_WORK" \
  node "$REPO/scripts/hosted-real-node.mjs"
status=$?
set -e
echo "screenshots: $FB_WORK/shots; desktops: $FB_WORK/host-a.log, host-b.log; relay: $FB_WORK/relay.log"
exit $status
