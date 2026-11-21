# Data Transfer

Copy tables between two open connections (or into an SQL script), in the
style of Navicat's Data Transfer wizard.

Open it from a connection's right-click menu, a table's right-click menu
or the arrows icon in a table tab's toolbar.

1. **Source / Target.** Pick the source connection and schema and either a
   target connection and schema or an SQL file. The swap button exchanges
   them. Save Profile / Load Profile keep a whole setup (profiles store
   connection *names*, never passwords) in `transfer-profiles.json` in the
   config directory. A loaded profile needs both connections to be open.
2. **Database Objects.** Tick the tables. Auto copies everything under the
   same name. Advanced sets a target name, custom fields (source to target
   mapping) and custom recordsets (WHERE conditions, or the Recordset
   Generator for integer and date ranges).
3. **Run.** Start, watch the log, Stop between batches.

## Safety

* A Read Only target is refused in the wizard and again by the worker.
* Drop target / Empty target (both off by default) and PROD targets ask for
  confirmation; dropping or emptying a PROD target needs the schema name
  typed.
* Names are quoted by the dialect, values are written as typed literals,
  recordset conditions must be one expression (no `;`, no comments).
* The log holds table names, counts and database error text; never row
  values, statements or passwords.
* With Use transaction (default) each table is copied in one transaction
  and rolled back when it fails or is stopped. Continue on error skips
  failing rows instead.

## How it works

`ora_core::transfer` holds the planner (`sqlgen`), the runner (`run`), the
generator (`ranges`) and profiles. The runner talks to `TransferIo`; the app
implements it over each connection's worker (`transfer_io.rs`), tests over
real SQLite databases, and `ScriptIo` writes an SQL script. Source rows are
read page by page ordered by row key (`ROWID` / `rowid`), so paging is
stable; a source without a row key falls back to offset paging.

## Not in this slice

Foreign keys, check constraints, triggers, defaults, primary key
constraints (unique indexes are copied), Lock source tables, CSV/XLSX file
targets, views and other object kinds, binary values over 2000 bytes into
Oracle, column types beyond a simple Oracle/SQLite mapping, and a transfer
between two Oracle databases has not been run against a real server.
