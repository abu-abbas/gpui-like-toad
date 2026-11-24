# Release targets and extension publishing

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

## Decide what to release

Fill in **Release impact** in every PR. Name the artifact users need to
update and give the reason. A merge makes the changes available to build.
The release workflows publish the artifacts.

| Release impact | When to use it | Workflow |
|---|---|---|
| No release needed | Documentation, tests, or workflow maintenance with no change to a shipped artifact | None |
| Kilat app only | App UI, a built-in tool, the extension host, or app packaging changes while existing extension binaries remain compatible | **Release** |
| Extension only | A separately downloadable driver or tool changes and the released app already supports it | **Release extension**, once per affected extension |
| Both | The change affects both shipped binaries, or the extension needs capabilities from a new Kilat build | **Release** and **Release extension** |

For a shared dependency change, check which shipped binaries actually use
the changed code. When both workflows are needed, record the order and
minimum app version in the PR. If an app release newly offers an extension
that is compatible with the current app, publish the signed extension and
verify its index entry before exposing Install in the app release.

Examples:

- MailHog Inbox styling in the Kilat renderer (PR #92) needs **Release**.
  The published MailHog 1.0.0 view is compatible.
- A PostgreSQL driver fix that keeps the same app protocol needs
  **Release extension** for `postgres`.
- A new extension feature with a required host change needs **both**.

## Prepare an extension release

For every affected downloadable extension, add a dated entry when the
version is published. Before publication, keep the planned version and
user-facing notes in an unreleased entry in its `RELEASE_NOTES.md`.
Use the release version from that entry for the manual workflow input.
Confirm the current version in the signed index and choose an unused
`X.Y.Z` tag.

| Extension input | Release notes |
|---|---|
| `sqlite` | [SQLite](../extensions/sqlite/RELEASE_NOTES.md) |
| `mailhog` | [MailHog](../extensions/mailhog/RELEASE_NOTES.md) |
| `postgres` | [PostgreSQL](../extensions/postgres/RELEASE_NOTES.md) |
| `mysql` | [MySQL](../extensions/mysql/RELEASE_NOTES.md) |
| `mssql` | [SQL Server](../extensions/mssql/RELEASE_NOTES.md) |
| `dryrun` | [Dry-run](../extensions/dryrun/RELEASE_NOTES.md) |

## Publishing

Actions > Release extension > Run workflow, pick the extension and a new
version (`X.Y.Z`). Paste the prepared notes in **Release notes**. The workflow
adds the build commit and install location to the release description. You
can also edit the release description on GitHub to add Markdown formatting.

The workflow checks the signing key, builds Linux x86_64,
Windows x86_64 and macOS arm64, checks that each program answers the
handshake, signs, verifies like the app does, publishes `ext-<id>-<version>`
and then rewrites the index. It refuses a version that already exists.
If it fails after the release was created but before the index was updated,
delete the `ext-<id>-<version>` release and its tag, then run it again.

A new extension needs `extensions/<id>/extension.json` (id, name,
description, kind `database`, `tool` or `action`, `crate`, `min_app`, optional
`max_app`), a crate `crates/ext-<id>` whose `main` calls
`ora_core::extension_serve::serve`, and its id added to the workflow's
`options`.

Set `min_app` to the oldest Kilat version compatible with this extension
release. Keep the value in its release notes so it is visible when
choosing what to publish.
