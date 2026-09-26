#!/usr/bin/env bash
# build-apt-repo.sh <site dir> <freebank_*.deb ...>
#
# Adds the given packages to the apt repository in <site dir> (the gh-pages checkout that GitHub Pages serves
# at https://apt.ecxfreebank.com) and re-signs it. Packages already in the pool are kept, so older versions
# stay installable. Layout:
#   pool/main/f/freebank/freebank_<ver>_<arch>.deb
#   dists/stable/{Release,InRelease,Release.gpg}
#   dists/stable/main/binary-<arch>/{Packages,Packages.gz}
#   freebank-archive-keyring.gpg (binary) and .asc, CNAME, index.html
#
# Signing: the key must already be in $GNUPGHOME; its fingerprint is taken from $APT_SIGNING_KEY.
set -euo pipefail

site=${1:?usage: build-apt-repo.sh <site dir> <deb ...>}
shift
[ "$#" -gt 0 ] || { echo "no packages given" >&2; exit 2; }
key=${APT_SIGNING_KEY:?set APT_SIGNING_KEY to the signing key fingerprint}
here=$(cd "$(dirname "$0")" && pwd)

pool="$site/pool/main/f/freebank"
mkdir -p "$pool"
for deb in "$@"; do
  name=$(dpkg-deb -f "$deb" Package)
  [ "$name" = freebank ] || { echo "$deb is package '$name', expected 'freebank'" >&2; exit 1; }
  target="$pool/freebank_$(dpkg-deb -f "$deb" Version)_$(dpkg-deb -f "$deb" Architecture).deb"
  if [ -e "$target" ]; then
    # A published version is never replaced: users may already have it, with its checksum.
    cmp -s "$deb" "$target" || { echo "$target is already published with different contents; bump the version" >&2; exit 1; }
    echo "$(basename "$target") already published, unchanged"
  else
    cp "$deb" "$target"
  fi
done

cd "$site"
archs=$(for d in pool/main/f/freebank/*.deb; do dpkg-deb -f "$d" Architecture; done | sort -u)
for arch in $archs; do
  dir="dists/stable/main/binary-$arch"
  mkdir -p "$dir/by-hash/SHA256"
  apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
  gzip -9nkf "$dir/Packages"
  # by-hash copies: GitHub Pages caches files for minutes, and apt fetches indexes by their hash when the
  # Release file says Acquire-By-Hash, so a cached old Packages can never pair with a new InRelease.
  for f in Packages Packages.gz; do
    cp "$dir/$f" "$dir/by-hash/SHA256/$(sha256sum "$dir/$f" | cut -d' ' -f1)"
  done
done

rm -f dists/stable/Release dists/stable/InRelease dists/stable/Release.gpg
apt-ftparchive \
  -o APT::FTPArchive::Release::Origin=FreeBank \
  -o APT::FTPArchive::Release::Label=FreeBank \
  -o APT::FTPArchive::Release::Suite=stable \
  -o APT::FTPArchive::Release::Codename=stable \
  -o APT::FTPArchive::Release::Components=main \
  -o "APT::FTPArchive::Release::Architectures=$(echo $archs)" \
  -o APT::FTPArchive::Release::Description="FreeBank desktop app" \
  -o APT::FTPArchive::Release::Acquire-By-Hash=yes \
  release dists/stable > dists/stable/Release.tmp
mv dists/stable/Release.tmp dists/stable/Release

gpg --batch --yes --local-user "$key" --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --local-user "$key" --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release

gpg --batch --yes --export "$key" > freebank-archive-keyring.gpg
gpg --batch --yes --armor --export "$key" > freebank-archive-keyring.asc

echo apt.ecxfreebank.com > CNAME
touch .nojekyll
fpr=$(gpg --batch --with-colons --fingerprint "$key" | awk -F: '$1 == "fpr" {print $10; exit}')
sed -e "s/@FINGERPRINT@/$fpr/g" "$here/../apt/index.html" > index.html

echo "apt repository in $site: $(ls pool/main/f/freebank | wc -l) package(s), signed by $fpr"
