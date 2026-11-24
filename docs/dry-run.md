# Dry-run (design and phase 1)

Dry-run runs an `UPDATE` or `DELETE` inside a transaction that is not
committed unless you press Commit, shows which rows would change (old value and new value), rolls
the transaction back (by hand, or by itself after 5 minutes) and writes an
undo script for the statement. It ships as the extension `dryrun`.

## Decision: how it fits the extension model
Existing extensions are either a database driver (`kind: database`, the app
asks it for rows) or a tool with its own server and screens (`kind: tool`,
a sidebar entry). Dry-run is neither: it has no sidebar entry and no
connection of its own in the explorer. It is something the user does to the
statement in the SQL editor.

So it becomes a third kind, `action`:

- The extension is a program like the others (same transport, handshake and
  signed download). It links the Oracle driver (`ora-driver`) and runs the
  statement in **its own Oracle session**. The app starts it with the active
  connection's profile and password (the same `connect` call a database
  extension gets) and sends the statement.
- The app side is small: a **Dry-run** button (flask icon) right after Run
  in the SQL editor toolbar (shown only when the extension is installed and
  the active connection is Oracle) and a **Dry-run** tab next to Messages in
  the editor's result pane that draws the extension's declarative view (the protocol of
  `extension-ui-protocol.md`, slot `main`). The diff, the warnings and the
  undo script are all view trees; the app has no Dry-run drawing code.
- **Commit** in the Dry-run tab (from extension 1.0.1, Kilat 0.1.65) commits
  the dry run's own session, so exactly the rows shown are written. It is a
  `writes` button: disabled on Read Only connections, confirmed on PROD, and
  Kilat records the statement in the history. (1.0.0 had "Run for real",
  which re-ran the statement in the editor instead.)

Why a separate session instead of the editor's session:

- The program that runs a statement and keeps a transaction open is a
  separate process, so a crash, a kill or a closed Kilat ends its session and
  Oracle rolls the transaction back by itself. No path leaves the editor's
  own transaction half changed.
- Nothing in the editor's connection (pending edits, open transaction) is
  touched.
- The cost: the dry-run does **not** see changes the user made in the editor
  session and has not committed yet. When the editor's connection has
  uncommitted changes, the Dry-run tab says so before it starts (with
  "Dry-run anyway"), and the capture uses `FOR UPDATE WAIT 10`: rows locked
  by another session stop the dry run after 10 seconds with a message to
  Commit or Roll back first, instead of waiting without end.

## Phase 1 scope
Assumptions (the mockup was approved as a direction only):

- Oracle only. Statements: `UPDATE` and `DELETE` of one table. `MERGE` and
  `INSERT` are refused with a clear message (phase 2: MERGE needs the target
  rows from the `USING` source).
- The target table is found by its name in the statement (`[schema.]table
  [alias]`), and the changed rows by `ROWID`.
- At most 1,000 rows are captured. The affected count is always exact (the
  database reports it). Above 1,000, the Dry-run tab shows the first 200 old/new
  rows and says that no undo script is written.
- At most 200 changed rows are drawn.
- The connection's password must be saved in the keychain: Kilat never keeps
  a typed password after connecting, and the extension needs one for its own
  session. Without it the button explains what to do. (Phase 2: ask for it.)
- The undo script has to fit one view string (60 KB); longer ones are not
  written, and values over 4,000 bytes are left out of it.

## What the extension does
1. Parse the statement (`dryrun_sql`): kind, `[schema.]table`, alias, the
   `WHERE` text. Anything unsupported (subquery in the table position,
   `RETURNING`, `MERGE`, more than one statement) is refused before the
   database is touched.
2. Count enabled triggers on the table. A trigger can do work a rollback
   does not undo (autonomous transactions, mail, database links), so the
   Dry-run tab warns.
3. Capture (and lock) the old rows:
   `SELECT ROWID, t.* FROM table t WHERE <where> FOR UPDATE WAIT 10`, reading
   at most 1,001 of them.
4. Run the statement in the session (no commit). The reported affected
   count is kept.
5. Read the rows again by `ROWID` (`UPDATE` only) and compare cell by cell.
   Deleted rows show as removed; an updated row that cannot be found again
   (row movement) is reported as "moved".
6. Build the view and the undo script (`UPDATE ... SET <changed columns> =
   <old> WHERE ROWID = '...'` for updates, `INSERT` for deletes). A row with a
   value that was clipped at the cell limit (large LOBs) is left out of the
   script and listed in a comment; undo is not promised for it.
7. Keep the transaction open until `dryrun.rollback`, a new `dryrun.run`,
   `disconnect`, process exit, or 5 minutes pass. A watchdog thread rolls
   back at the deadline.

Known limits, shown in the Dry-run tab:

- Rows another session changes between step 3 and step 4 can make "old" differ
  from what the statement saw (no serializable transaction is used).
- The statement holds row locks in its session until the rollback. This is
  what a real run would do too, but for the dry-run window only.
- Sequences advance, and side effects outside the transaction (autonomous
  transactions, `UTL_*`, database links) are not undone.
- Implicit-commit statements (DDL) are never accepted.

## Protocol
Extension methods (after `hello`):

| method | params | result |
|---|---|---|
| `connect` | `profile`, `password` | `{user, database, version}` |
| `disconnect` | none | `{}`; rolls back |
| `dryrun.run` | `sql` | `{affected, state}` and the view is ready for `ui.open` |
| `dryrun.rollback` | none | `{}` |
| `ui.open` | `slot: "main"` | view |
| `ui.event` | button ids `rollback`, `commit`, `copy-undo`; tabs `view` | view and effects |
| `ui.tick` | none | view (countdown, expired) |

`commit` is declared with `writes: true`, so the app's Read Only and PROD
guards apply before the event reaches the extension; the extension also
refuses it unless a dry run is waiting.

## Release
1.0.1 needs Kilat 0.1.65 (new view components, Commit, Dry-run tab). 1.0.0 needed an app release (new kind `action`, toolbar button, Dry-run tab) **and** an
extension release (`dryrun` 1.0.0, `min_app` the app version that ships the
button). Order: app first, then the extension.
