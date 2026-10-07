#!/bin/bash
# A regtest chain for the hosted-wallets end to end (scripts/hosted-real-node.sh), set up in the folder this script
# is copied to (it uses its parent for the v0.2.19 release binaries in fb219rel/; adjust FB_ROOT to taste): the standing stack with two
# freebankd v0.2.19 nodes; node A's main wallet runs a members-only house "stall", holds 1,000,000 units of its
# notes and is encrypted (passphrase in a.pass); node B is a fresh node. Writes env.sh, restart.sh, bmm1.sh, go.sh.
# Needs FB_NODE_REPO (the freebank node's checkout, for its standing stack), FB_DIST (the distribution checkout, for
# the phone page and the relay) and FB_APP (this app's checkout).
set -euo pipefail
: "${FB_NODE_REPO:?set FB_NODE_REPO to the freebank node checkout}" "${FB_DIST:?set FB_DIST to the distribution checkout}"
: "${FB_APP:?set FB_APP to the app checkout}"
E=$(cd "$(dirname "$0")" && pwd)
S=$(dirname "$E")
cd "$E"
rm -rf fb-gate.* cookie-a cookie-b
(TMPDIR=$E FB_ROOT=$S/fb219rel setsid "$FB_NODE_REPO/test/integration/lib/standing_stack.sh" --nodes 2 --deposit 40 > stack.log 2>&1 & echo $! > stack.pid)
for i in $(seq 1 300); do ls fb-gate.*/stack.env >/dev/null 2>&1 && grep -q 'N2_CLI' fb-gate.*/stack.env && break; sleep 2; done
W=$(ls -d fb-gate.*)
cat > env.sh <<ENV
source $E/$W/stack.env
G=\$HOME/.local/bin/grpcurl
FB=\$FB_CLI; N2=\$N2_CLI
enf() { timeout 30 \$G -plaintext -d "\${2:-{\\}}" \$ENFORCER_GRPC "cusf.mainchain.v1.\$1"; }
MA=\$(enf WalletService/CreateNewAddress | sed -n 's/.*"address": *"\([^"]*\)".*/\1/p')
l1() { enf MiningService/GenerateToAddress "{\"blocks\":\$1,\"address\":\"\$MA\"}" >/dev/null; }
bmm() { local t=\$(( \$(\$FB getblockcount) + \${1:-1} )) n=0; while [ "\$(\$FB getblockcount)" -lt "\$t" ]; do \$FB refreshbmm 0.001 >/dev/null 2>&1; l1 1; sleep 0.3; n=\$((n+1)); [ \$n -gt 100 ] && { echo "bmm stuck"; return 1; }; done; \$FB refreshbmm 0.001 >/dev/null 2>&1; true; }
ENV
source ./env.sh
for i in $(seq 1 60); do [ "$($FB getbalance 2>/dev/null | cut -d. -f1)" -ge 39 ] 2>/dev/null && break; bmm 1; sleep 1; done
echo "A balance $($FB getbalance)"
# The nodes' command lines, for restarts.
APORT=$(grep -o -- '-rpcport=[0-9]*' <<<"$FB_CLI" | cut -d= -f2); BPORT=$(grep -o -- '-rpcport=[0-9]*' <<<"$N2_CLI" | cut -d= -f2)
echo "ports A $APORT B $BPORT"
for n in a b; do
  port=$([ $n = a ] && echo $APORT || echo $BPORT)
  pid=$(pgrep -f "freebankd.*-rpcport=$port" | head -1)
  tr '\0' '\n' < /proc/$pid/cmdline > $n.cmdline
done
cat > restart.sh <<RS
#!/bin/bash
# restart.sh <a|b>: start that node again with the command line it had, and wait for its RPC.
source $E/env.sh >/dev/null 2>&1
mapfile -t A < "$E/\$1.cmdline"
"\${A[@]}" >/dev/null 2>&1
if [ "\$1" = a ]; then CLI=\$FB_CLI; else CLI=\$N2_CLI; fi
for i in \$(seq 1 120); do \$CLI getblockcount >/dev/null 2>&1 && exit 0; sleep 0.5; done
exit 1
RS
printf '#!/bin/bash\nsource %s/env.sh >/dev/null 2>&1\nbmm 1\n' "$E" > bmm1.sh
chmod +x restart.sh bmm1.sh
# The house.
$FB registerhouse 0 1 "stall" 1000 "[1.0]" 0.001 "members" >/dev/null
bmm 6
H=$($FB listhouses | python3 -c "import json,sys; print([h['id'] for h in json.load(sys.stdin) if h['classid']=='stall'][0])")
$FB attesthouse $H 0.001 >/dev/null
bmm 2
for i in 1 2 3 4 5 6; do $FB sendtoaddress "$($FB getnewaddress '' legacy)" 0.5 >/dev/null; done
bmm 2
$FB mintnote $H 1000000 0.001 >/dev/null
bmm 2
echo "house $H; A holds $($FB listmynotes | python3 -c "import json,sys; print(sum(n['units'] for n in json.load(sys.stdin)))") units"
# Passphrases (never printed), and node A's wallet encrypted (the node stops; started again).
umask 077
head -c 24 /dev/urandom | base64 | tr -d '/+=' > a.pass
head -c 24 /dev/urandom | base64 | tr -d '/+=' > b.pass
$FB encryptwallet "$(cat a.pass)" >/dev/null 2>&1 || true
for i in $(seq 1 60); do $FB getblockcount >/dev/null 2>&1 || break; sleep 1; done
sleep 2
./restart.sh a
mkdir -p cookie-a cookie-b
echo "t:t" > cookie-a/.cookie; echo "t:t" > cookie-b/.cookie
cat > go.sh <<GO
#!/bin/bash
E=$E
source "\$E/env.sh" >/dev/null 2>&1
export FB_A_URL=http://127.0.0.1:$APORT FB_A_COOKIE=\$E/cookie-a/.cookie FB_A_CLI="\$FB_CLI" FB_A_PASS_FILE=\$E/a.pass
export FB_A_RESTART_CMD="\$E/restart.sh a" FB_A_DATADIR=\$FB_DATADIR
export FB_B_URL=http://127.0.0.1:$BPORT FB_B_COOKIE=\$E/cookie-b/.cookie FB_B_CLI="\$N2_CLI" FB_B_PASS_FILE=\$E/b.pass
export FB_B_RESTART_CMD="\$E/restart.sh b"
export FB_BMM_CMD=\$E/bmm1.sh FB_DIST=$FB_DIST FB_WORK=\$E/run
exec $FB_APP/scripts/hosted-real-node.sh
GO
chmod +x go.sh
mkdir -p "$E/run"; echo "$H" > "$E/run/house"
echo "SETUP DONE: A wallet encrypted: $($FB getwalletinfo | grep -c unlocked_until)"
