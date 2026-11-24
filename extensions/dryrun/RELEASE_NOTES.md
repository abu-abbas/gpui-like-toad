# Dry-run extension release notes

## [1.0.1](https://github.com/abu-abbas/kilat/releases/tag/ext-dryrun-1.0.1) · 2026-11-25

Minimum Kilat version `0.1.65`.

- New look in the Dry-run tab next to Messages: a yellow strip with the row count, how long the rows have been locked and the auto rollback countdown, with Roll back and Commit right there.
- Commit writes exactly the rows the dry run shows (from its own session); on PROD it asks first and on Read Only connections it is disabled. After a commit the strip turns green and the undo script is one click away.
- Changed values show the old value struck out in red next to the new value in green. Deleted rows are struck out.
- Triggers on the table are listed as a side effect, with a note that rows they change are not shown.
- Dry-run no longer waits forever when the rows are locked by another session that has not committed yet (often the same connection in the SQL editor or a data tab). After 10 seconds it stops and says to Commit or Roll back there first.
- The trigger warning works again for tables without a schema prefix.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-dryrun-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.63`.

- First Dry-run package. Install it from Settings → Extensions, then use the new Dry-run button next to Run in the SQL editor.
- Dry-run runs one `UPDATE` or `DELETE` of a single Oracle table in its own session and never commits. It shows which rows would change with the old and new values, how many rows are affected, and a warning when the table has enabled triggers.
- The transaction rolls back by itself after 5 minutes, or at once with Roll back now. Closing Kilat or the connection also rolls it back.
- Copy an undo script (`UPDATE ... WHERE ROWID` for updates, `INSERT` for deletes) to restore the old values after you run the statement for real. No script is written above 1,000 rows or for values over 4,000 bytes.
- Needs the connection's password saved in the keychain. `MERGE` and `INSERT` are not supported yet.
