# Releasing an extension

Extensions are separate programs (see `extension-protocol.md`) published from
this repo's Releases, next to the app. Two kinds of release exist besides the
app's `v<version>`:

- `ext-<id>-<version>`: the program for each platform
  (`kilat-ext-<id>-<version>-<os>-<arch>`, `.exe` on Windows), `SHA256SUMS`
  and a `.minisig` for every file. Signed with the app's release key, with
  the trusted comment `version:<version>`.
- `extensions-index`: `extensions.json` and `extensions.json.minisig`. The
  index lists the newest version of every extension. Its trusted comment is
  `version:<revision>`, where `revision` is a counter inside the file that
  goes up by one on every release, so an older signed index cannot replace a
  newer one.

None of them is ever marked "latest" (the app updater reads
`releases/latest`), and the app's release pruning only touches `v<number>`
tags.

## Publishing

Actions > Release extension > Run workflow, pick the extension and a new
version (`X.Y.Z`). The workflow checks the signing key, builds Linux x86_64,
Windows x86_64 and macOS arm64, checks that each program answers the
handshake, signs, verifies like the app does, publishes `ext-<id>-<version>`
and then rewrites the index. It refuses a version that already exists.
If it fails after the release was created but before the index was updated,
delete the `ext-<id>-<version>` release and its tag, then run it again.

A new extension needs `extensions/<id>/extension.json` (id, name,
description, kind `database` or `tool`, `crate`, `min_app`, optional
`max_app`), a crate `crates/ext-<id>` whose `main` calls
`ora_core::extension_serve::serve`, and its id added to the workflow's
`options`.

`min_app` must be the first app version that can run extensions.
