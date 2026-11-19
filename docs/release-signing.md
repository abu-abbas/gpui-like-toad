# Tanda tangan rilis (tanpa Apple Developer ID)

Rilis dari workflow **Release** membawa:

- `SHA256SUMS`: checksum semua file rilis.
- `<file>.minisig` dan `SHA256SUMS.minisig`: tanda tangan [minisign](https://jedisct1.github.io/minisign/)
  dengan trusted comment `version:X.Y.Z`. **Wajib**: job `preflight` (sebelum build)
  dan job `publish` (sebelum tanda tangan) menjalankan
  `.github/scripts/check-release-key.sh`, yang menghentikan workflow kalau secret
  `MINISIGN_SECRET_KEY` kosong, tidak bisa dibaca (rusak/berpassword), atau kunci
  publiknya tidak sama dengan `release-key.pub`. Setelah tanda tangan,
  `.github/scripts/verify-release.sh` memeriksa semua file dengan `release-key.pub` dari
  repo sebelum rilis diterbitkan.
- Bundel `.app` macOS bertanda tangan **ad-hoc** (`codesign -s -`). Ini bukan
  notarization: instalasi pertama dari browser tetap perlu klik kanan > Open sekali.

## Apa yang diperiksa app

Tombol **Download** dan **Update Now** memakai verifier yang sama
(`crates/app/src/release_verify.rs`). File diunduh ke folder temp yang hanya bisa
dibaca user itu, lalu diterima hanya kalau:

1. `SHA256SUMS` valid untuk `SHA256SUMS.minisig` dengan kunci `release-key.pub`, dan
   trusted comment-nya `version:<versi rilis>`;
2. SHA-256 file sama dengan barisnya di `SHA256SUMS`;
3. `<file>.minisig` valid untuk file itu dengan kunci yang sama, juga dengan
   `version:<versi rilis>`.

Rilis yang tidak punya salah satunya ditolak sebelum file diunduh. Download baru
disalin ke folder Downloads setelah lolos (sebagai `.part`, dicek ulang, lalu
di-rename); Update Now baru dipasang setelah lolos. Format rilis tidak berubah, jadi
updater versi lama (yang membaca `SHA256SUMS` + `<file>.minisig`) tetap menerima rilis
baru.

Kunci publik ada di `release-key.pub` (di-commit setelah kunci dibuat, lihat bawah). Verifikasi manual:

```sh
minisign -V -p release-key.pub -m Kilat-X.Y.Z-linux-x86_64
```

## Membuat kunci (sekali saja)

Pasang minisign dulu: `brew install minisign` (macOS), `sudo apt install minisign`
(Ubuntu), atau `scoop install minisign` (Windows).

```sh
minisign -G -W -p release-key.pub -s release.key
```

1. Simpan **isi** `release.key` sebagai secret repo `MINISIGN_SECRET_KEY`
   (Settings > Secrets and variables > Actions), lalu hapus file `release.key` dari
   komputer atau simpan di password manager. Jangan di-commit.
2. Commit `release-key.pub` ke repo. Kunci publik
   inilah yang nanti ditanam di app untuk memverifikasi update.
3. Kalau kunci rahasia bocor atau hilang: buat pasangan kunci baru dan rilis app
   dengan kunci publik baru (app lama tidak bisa memverifikasi rilis berikutnya).
