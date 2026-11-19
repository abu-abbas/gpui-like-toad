#!/usr/bin/env bash
# Periksa folder rilis persis seperti app (Download dan Update Now) dan
# updater versi lama memeriksanya, dengan release-key.pub dari repo (bukan
# kunci turunan dari secret): SHA256SUMS cocok, setiap file punya .minisig
# yang valid, dan trusted comment-nya `version:<versi>`.
#
# Pemakaian: verify-release.sh <folder> <versi X.Y.Z> <release-key.pub>
set -euo pipefail

dir=${1:?folder}
version=${2:?versi}
pub=$(cd "$(dirname "${3:?release-key.pub}")" && pwd)/$(basename "$3")
cd "$dir"

fail() {
  echo "::error title=Verifikasi rilis::$*"
  exit 1
}

[ -f SHA256SUMS ] || fail "SHA256SUMS tidak ada."
sha256sum --quiet -c SHA256SUMS || fail "SHA256SUMS tidak cocok dengan file."
listed=$(awk '{ sub(/^\*/, "", $2); print $2 }' SHA256SUMS | sort)
present=$(ls Kilat-* | grep -v '\.minisig$' | sort)
[ "$listed" = "$present" ] || fail "SHA256SUMS tidak memuat semua file rilis."

for f in $present SHA256SUMS; do
  [ -f "$f.minisig" ] || fail "$f.minisig tidak ada."
  out=$(minisign -V -p "$pub" -m "$f" 2>&1) || fail "Tanda tangan $f tidak valid untuk release-key.pub: $out"
  printf '%s\n' "$out" | grep -qx "Trusted comment: version:$version" \
    || fail "Tanda tangan $f tidak menyebut version:$version."
done
echo "Semua $(echo "$present" | wc -l) file + SHA256SUMS terverifikasi dengan release-key.pub (versi $version)."
