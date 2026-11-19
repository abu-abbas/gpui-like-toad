# Tanda tangan rilis (tanpa Apple Developer ID)

Rilis dari workflow **Release** membawa:

- `SHA256SUMS`: checksum semua file rilis.
- `<file>.minisig` dan `SHA256SUMS.minisig`: tanda tangan [minisign](https://jedisct1.github.io/minisign/)
  (hanya kalau secret `MINISIGN_SECRET_KEY` diisi; kalau kosong langkahnya dilewati
  dengan peringatan).
- Bundel `.app` macOS bertanda tangan **ad-hoc** (`codesign -s -`). Ini bukan
  notarization: instalasi pertama dari browser tetap perlu klik kanan > Open sekali.

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
