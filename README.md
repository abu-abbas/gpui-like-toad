# gpui-like-toad

Semacam Toad for Oracle, tapi ditulis ulang di Rust dengan [gpui-kit](https://gpui-kit.com) — ringan, satu binary, tanpa install Oracle Client.

> Status: MVP. Connection manager, schema browser, SQL editor + run, result grid, export CSV, dan explain plan sudah ada. GUI-nya sendiri belum pernah dikompilasi di VM ini (RAM tidak cukup); kompilasi GUI pertama kali lewat GitHub Actions, lihat [Verifikasi](#verifikasi).

## Fitur MVP

- **Connection manager** — form koneksi (nama, username, connect string, password) + mode **Demo** yang jalan tanpa database sama sekali.
- **Schema browser** — tree objek per owner (`ALL_OBJECTS`): tables, views, indexes, sequences, procedures, functions, packages, triggers, synonyms. Klik tabel/view untuk DESCRIBE.
- **SQL editor** — syntax highlighting SQL, nomor baris, folding. Run query (SELECT) atau statement lain (DML/DDL/PL/SQL) dari tombol Run.
- **Result grid** — virtual scrolling, kolom bisa di-resize, klik header untuk sort. Hasil dibatasi 1.000 baris dan UI-nya bilang kalau terpotong.
- **Export CSV** — hasil terakhir diekspor ke `gpui-like-toad-export-<unix>.csv` di working directory.
- **Explain plan** — `EXPLAIN PLAN` + `DBMS_XPLAN.DISPLAY`, tampil sebagai teks.
- **Commit / Rollback** — eksplisit, tidak ada auto-commit.

Yang sengaja belum ada di MVP: dock layout yang bisa di-drag, shortcut F5/F9, save dialog native, autocomplete, session browser, PL/SQL debugger. Semuanya tercatat sebagai follow-up, bukan lupa.

## Cara jalan

### Prasyarat

- Rust 1.92+ (`rustup update stable`).
- Linux (Ubuntu 24.04, sesuai panduan gpui-kit):

  ```sh
  sudo apt update
  sudo apt install -y gcc g++ clang libfontconfig-dev libwayland-dev \
    libwebkit2gtk-4.1-dev libxkbcommon-x11-dev libx11-xcb-dev \
    libssl-dev libzstd-dev libasound2-dev vulkan-validationlayers libvulkan1
  ```

  Jalan di sesi grafis Wayland/X11 dengan driver Vulkan yang berfungsi.
- Windows: Visual Studio 2022 Build Tools/Community workload **Desktop development with C++** (toolchain MSVC, bukan GNU) + CMake di `PATH`.
- macOS 15+: Xcode Command Line Tools.

Tidak perlu Oracle Client / Instant Client: driver-nya thin mode, ngomong langsung protokol Oracle Net.

### Build & run

```sh
cargo run -p gpui-like-toad
```

Build pertama lama (gpui-kit besar); build berikutnya jauh lebih cepat. Profil dev di workspace sudah mengoptimalkan crate framework-nya supaya aplikasi tetap responsif saat development.

### Mode Demo (tanpa database)

Klik **Demo** di panel kiri, lalu **Run**. Editor sudah berisi `SELECT * FROM employees` yang dijawab oleh database demo in-memory (tabel `EMPLOYEES`, `DEPARTMENTS`, `DUAL`). Semua fitur — schema tree, describe, explain, export — bisa dicoba tanpa Oracle sama sekali.

### Konek ke Oracle

Isi username, connect string, dan password, lalu **Connect**. Connect string bisa:

- Easy Connect: `dbhost:1521/orclpdb`
- Connect descriptor penuh: `(DESCRIPTION=(ADDRESS=...)(CONNECT_DATA=(SERVICE_NAME=...)))`
- Alias `tnsnames.ora` (driver membaca file konfigurasinya)

Password tidak pernah disimpan ke disk — hanya hidup di memori selama sesi berjalan. Tidak ada penyimpanan profil di MVP ini; itu follow-up (rencananya pakai keychain OS, bukan file plaintext).

Catatan: statement tidak boleh diakhiri `;` atau `/` saat dikirim ke driver — aplikasi otomatis membersihkannya sebelum eksekusi.

## Font & ligature

- **Editor** memakai **JetBrains Mono** v2.304 yang di-bundle di `crates/app/assets/fonts/` (lisensi OFL, lihat `OFL.txt` di folder yang sama) — tampilannya sama di semua mesin, font terinstall atau tidak.
- **Ligature kode** (`->`, `=>`, `!=`, `===`, ...) aktif: text system membiarkan fitur OpenType `calt` menyala secara default, dan tidak ada jalur di editor yang mematikannya.
- **UI** memakai **JetBrains Sans** kalau terinstall di sistem (misalnya bawaan JetBrains IDE); kalau tidak ada, fallback ke font default platform. JetBrains Sans tidak di-bundle karena tidak ada rilis OFL-nya — hanya tersedia sebagai webfont dari CDN JetBrains.

## Arsitektur

Workspace tiga crate, supaya logika database bisa dites tanpa GUI:

```
crates/
  ora-core/    # Tanpa GUI, tanpa dependensi: trait DbClient, model hasil,
               # export CSV, helper SQL, backend demo in-memory.
  ora-driver/  # Implementasi DbClient di atas driver resmi Oracle
               # (rust-oracledb, thin mode). Satu-satunya crate yang
               # tahu soal driver.
  app/         # GUI gpui-kit: workbench, worker thread database,
               # adapter result grid, schema tree.
```

Alur database: UI → `Job` (channel) → satu worker thread yang memiliki `DbClient` → `Response` → UI. Driver bersifat blocking, jadi tidak pernah dipanggil dari UI thread.

## Driver Oracle: kenapa git dependency

Per 16 November 2026, **semua** versi crate `oracledb` di crates.io berstatus *yanked* (crate independen sedang diserahterimakan ke Oracle), dan tidak ada crate bernama `rust-oracledb`. Satu-satunya sumber yang bisa di-resolve saat ini adalah repo resmi Oracle:

```toml
oracledb = { git = "https://github.com/oracle/rust-oracledb", rev = "b1adfff18b7240bfbe82b5148c0f114c6471feb6" }
```

Revisi di-pin ke commit yang diverifikasi tanggal tersebut (`26.0.0-beta.7`, pre-release). Kalau Oracle menerbitkan versi crates.io yang tidak di-yanked, dependensi ini bisa diganti ke versi crates.io tanpa mengubah kode lain — `DbClient` mengisolasi GUI dari driver.

## Verifikasi

Yang sudah dibuktikan mesin (bukan klaim):

- `cargo test -p ora-core -p ora-driver` hijau di VM dev: 21 test (model, CSV, helper SQL, backend demo, parsing connect string, error path driver).
- `cargo clippy -p ora-core -p ora-driver -- -D warnings` hijau.
- `cargo check -p ora-driver` hijau — glue code driver terkompilasi, termasuk konversi tipe per `DB_TYPE_*`.
- Unit test app crate (sorting grid, schema tree, nama file export) hanya jalan di CI — mengompilasinya butuh gpui-kit yang tidak muat di RAM VM ini.
- GUI (`cargo check -p gpui-like-toad`) **belum** terkompilasi di VM ini: dependensi gpui-kit butuh RAM jauh di atas yang tersedia. Kompilasi GUI pertama kali lewat CI (Ubuntu + Windows + macOS) di setiap push/PR; statusnya terlihat di tab Actions repo ini. Kode GUI ditulis mengikuti API gpui-kit 0.9.1 yang diverifikasi dari source crate-nya, tapi kejujuran batas ini penting: sampai CI hijau, GUI belum terbukti kompil.

CI (`ci.yml`) meniru pola Sistem Akademis: deteksi perubahan, cek judul PR (Conventional Commits), job test headless, job GUI per OS, Semgrep, dan agregator `ci-ok` sebagai satu-satunya required check.

## Rilis

Pola rilis meniru Sistem Akademis: setiap push ke `main` yang menyentuh kode (atau manual via workflow_dispatch) membangun biner rilis Linux + Windows + macOS (ARM), lalu menerbitkannya sebagai GitHub Release di repo publik terpisah **`abu-abbas/gpui-like-toad-releases`**, versi `0.1.<run_number>`, 5 rilis terakhir dipertahankan. Setiap biner di-smoke-test dulu (`--version` harus mencetak versi yang sama dengan nomor rilis).

Setup sekali (oleh pemilik repo):

1. Buat repo publik `abu-abbas/gpui-like-toad-releases` (kosong, tanpa README).
2. Buat fine-grained PAT dengan akses **Contents: Read and write** ke repo rilis tersebut saja.
3. Tambahkan sebagai secret `RELEASES_TOKEN` di repo `gpui-like-toad` (Settings → Secrets and variables → Actions).

Kalau secret belum diisi, workflow rilis hanya memberi warning dan dilewati — tidak merah. Biner belum ditandatangani; code signing menyusul.

## Roadmap (follow-up)

- Dock layout (panel bisa di-drag/tab) pakai komponen Dock gpui-kit.
- Shortcut Run (F5) dan Explain.
- Save dialog native untuk export CSV.
- Autocomplete nama tabel/kolom di editor.
- Penyimpanan profil koneksi terenkripsi (keychain OS).
- Fetch JSON/VECTOR sebagai teks JSON (saat ini tampil format Debug).
- Riwayat query.

## Lisensi

MIT.
