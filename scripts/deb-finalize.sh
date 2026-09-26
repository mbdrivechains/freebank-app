#!/usr/bin/env bash
# deb-finalize.sh <tauri .deb> <out dir> [keyring.gpg]
#
# Tauri names the Debian package after productName in kebab case ("free-bank") and the desktop file
# "FreeBank.desktop". This repacks the .deb so that:
#   - the package is called "freebank", so `sudo apt install freebank` works;
#   - the desktop file is named after the app id, com.ecxfreebank.freebank.desktop, which is what GNOME
#     matches a running window to (so the dock shows the FreeBank icon, not a generic one);
#   - it takes over from an earlier "free-bank" install;
#   - it ships the apt repository's public key at /usr/share/keyrings/freebank-archive-keyring.gpg, the path
#     the install instructions use, so a key extension or rotation reaches users with the next update.
# Writes <out dir>/freebank_<version>_<arch>.deb and prints its path.
set -euo pipefail

in=${1:?usage: deb-finalize.sh <tauri .deb> <out dir>}
out=${2:?usage: deb-finalize.sh <tauri .deb> <out dir>}
keyring=${3:-$(cd "$(dirname "$0")/.." && pwd)/apt/freebank-archive-keyring.gpg}
app_id=com.ecxfreebank.freebank

work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

dpkg-deb -R "$in" "$work/pkg"
ctl="$work/pkg/DEBIAN/control"

version=$(awk -F': ' '$1 == "Version" {print $2}' "$ctl")
arch=$(awk -F': ' '$1 == "Architecture" {print $2}' "$ctl")

# Package name, and the takeover of an earlier package name.
sed -i -e 's/^Package: .*/Package: freebank/' "$ctl"
grep -q '^Replaces:' "$ctl" || printf 'Replaces: free-bank\n' >> "$ctl"
grep -q '^Conflicts:' "$ctl" || printf 'Conflicts: free-bank\n' >> "$ctl"
grep -q '^Homepage:' "$ctl" || printf 'Homepage: https://github.com/mbdrivechains/freebank-app\n' >> "$ctl"
sed -i -e 's/^Maintainer: FreeBank$/Maintainer: FreeBank <apt@ecxfreebank.com>/' "$ctl"
# Tauri leaves a "(none)" long description; give it a real one.
if grep -q '^ (none)$' "$ctl"; then
  sed -i -e 's/^ (none)$/ Finds the eCash beta node and enforcer on this computer or your network, installs\n and runs a FreeBank node beside them, and shows its peers and blocks./' "$ctl"
fi

# Desktop file named after the app id; the category makes it show under Office/Finance.
apps="$work/pkg/usr/share/applications"
if [ -f "$apps/FreeBank.desktop" ]; then
  mv "$apps/FreeBank.desktop" "$apps/$app_id.desktop"
fi
sed -i -e 's/^Categories=$/Categories=Office;Finance;/' "$apps/$app_id.desktop"

# The repository key, so apt keeps trusting the repository across key changes.
install -D -m 644 "$keyring" "$work/pkg/usr/share/keyrings/freebank-archive-keyring.gpg"

# dpkg checks installed files against md5sums; rebuild it after the changes above.
(cd "$work/pkg" && find . -path ./DEBIAN -prune -o -type f -printf '%P\0' | LC_ALL=C sort -z | xargs -0 md5sum) \
  > "$work/pkg/DEBIAN/md5sums"

mkdir -p "$out"
dest="$out/freebank_${version}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz -b "$work/pkg" "$dest" >/dev/null
echo "$dest"
