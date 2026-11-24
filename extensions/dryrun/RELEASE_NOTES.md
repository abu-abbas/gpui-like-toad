# Dry-run extension release notes

## 1.0.0 · Unreleased

Minimum Kilat version `0.1.63`.

- First Dry-run package. Install it from Settings → Extensions, then use the new Dry-run button next to Run in the SQL editor.
- Dry-run runs one `UPDATE` or `DELETE` of a single Oracle table in its own session and never commits. It shows which rows would change with the old and new values, how many rows are affected, and a warning when the table has enabled triggers.
- The transaction rolls back by itself after 5 minutes, or at once with Roll back now. Closing Kilat or the connection also rolls it back.
- Copy an undo script (`UPDATE ... WHERE ROWID` for updates, `INSERT` for deletes) to restore the old values after you run the statement for real. No script is written above 1,000 rows or for values over 4,000 bytes.
- Needs the connection's password saved in the keychain. `MERGE` and `INSERT` are not supported yet.
