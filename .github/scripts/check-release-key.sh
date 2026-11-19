#!/usr/bin/env bash
# Pastikan secret MINISIGN_SECRET_KEY terisi, bisa dibaca tanpa password,
# dan kunci publiknya sama dengan release-key.pub yang dibundel di app.
# Kalau tidak, rilis dihentikan: rilis tanpa tanda tangan atau bertanda
# tangan kunci lain akan ditolak oleh tombol Download dan Update Now.
#
# Pemakaian: check-release-key.sh <release-key.pub> <file-kunci-rahasia-keluaran>
# Kunci rahasia dibaca dari env MINISIGN_SECRET_KEY, ditulis ke argumen
# kedua (mode 600) supaya langkah tanda tangan bisa memakainya; kunci
# publik turunannya ditulis ke <argumen kedua>.pub.
set -euo pipefail

app_pub=${1:?release-key.pub}
key_out=${2:?file kunci keluaran}

fail() {
  echo "::error title=Kunci rilis::$*"
  exit 1
}

if [ -z "${MINISIGN_SECRET_KEY//[[:space:]]/}" ]; then
  fail "Secret MINISIGN_SECRET_KEY kosong. Isi di Settings > Secrets and variables > Actions; rilis tidak diterbitkan tanpa tanda tangan."
fi
[ -f "$app_pub" ] || fail "$app_pub tidak ada di repo."
command -v minisign >/dev/null || fail "minisign belum terpasang."

umask 077
printf '%s\n' "$MINISIGN_SECRET_KEY" > "$key_out"
# </dev/null: kunci berpassword akan meminta password dan gagal, bukan
# menggantung.
if ! minisign -R -s "$key_out" -p "$key_out.pub" </dev/null >/dev/null 2>&1; then
  rm -f "$key_out" "$key_out.pub"
  fail "MINISIGN_SECRET_KEY tidak bisa dibaca (format rusak atau berpassword; buat dengan minisign -G -W)."
fi

key_line() { tail -n 1 "$1" | tr -d '[:space:]'; }
key_id() { head -n 1 "$1" | sed -n 's/.*public key //p'; }
want=$(key_line "$app_pub")
got=$(key_line "$key_out.pub")
if [ -z "$want" ] || [ "$want" != "$got" ]; then
  got_id=$(key_id "$key_out.pub")
  rm -f "$key_out" "$key_out.pub"
  fail "MINISIGN_SECRET_KEY (key id ${got_id:-?}) tidak cocok dengan $app_pub (key id $(key_id "$app_pub")). App akan menolak rilis ini."
fi
echo "Kunci rilis cocok dengan $app_pub (key id $(key_id "$app_pub"))."
