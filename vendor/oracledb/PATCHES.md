# Vendored rust-oracledb

Copy of https://github.com/oracle/rust-oracledb at revision
`b1adfff18b7240bfbe82b5148c0f114c6471feb6` (crate `oracledb` 26.0.0-beta.7),
UPL-1.0 OR Apache-2.0 (see LICENSE.txt). Only `src/`, `doc/` (included by
`src/lib.rs` as rustdoc), the manifest and license files are kept; the
examples, tests and dev-dependencies were dropped.

## Local patches

1. **10G password verifier** (`src/messages/auth.rs`, `src/constants.rs`,
   `des` dependency in `Cargo.toml`). Upstream thin mode rejects accounts
   whose only password hash is the pre-11g DES one with
   `not implemented: password verifier type 0x939`. Such accounts still exist
   on databases that were upgraded from 10g and never had the password reset.
   The patch adds `VERIFIER_TYPE_10G`, the DES-based hash
   (`password_hash_10g`, checked against published SCOTT/TIGER,
   SYSTEM/MANAGER and SYS/CHANGE_ON_INSTALL values) and an AES-128 O5LOGON
   path (`generate_verifier_10g`; combo key via PBKDF2 when the server sends
   `AUTH_PBKDF2_CSK_SALT`, as 12c+ servers do, else MD5). The 10G hash is
   case-insensitive, so the
   user name and password are upper-cased.

When moving to a newer upstream revision, re-apply these patches (search for
"Patched (gpui-like-toad)") or drop them if upstream gains 10G support.

2. **Authentication metadata validation** (`src/messages/auth.rs`,
   `validate_server_params`). Upstream `unwrap()`s the server's
   `AUTH_SESSKEY`, `AUTH_VFR_DATA`, `AUTH_PBKDF2_*` values, so a garbled or
   hostile response panicked or ran an unbounded PBKDF2. They are now checked
   (present, hex, AES block multiple, iteration counts 1..=10,000,000) when
   the verifier response is parsed, and a bad value is a normal connect
   error. Unit tests only; not run against a real server.
