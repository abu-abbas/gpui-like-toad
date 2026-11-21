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

## Constraints, defaults, triggers, lock, CSV

* Primary key, UNIQUE, CHECK and foreign keys are read per table
  (`DbClient::list_constraints`: `ALL_CONSTRAINTS` on Oracle, pragmas and the
  stored `CREATE TABLE` on SQLite) and created unnamed, so names never
  collide inside a schema. Primary key, unique and check go into
  `CREATE TABLE`. Foreign keys go into `CREATE TABLE` on SQLite and are
  added with `ALTER TABLE` after every table is copied on Oracle, and only
  when the referenced table is part of the same transfer with all its
  columns; otherwise the log says why it was skipped. An index that only
  backs a copied key is not created twice.
* Check expressions and default values are copied only between the same kind
  of database, one plain expression each (no sequences, no schema-qualified
  names, not when columns were renamed or left out).
* Triggers (off by default) are copied after the rows, between the same kind
  of database, for tables that keep their name and all columns; a failing
  trigger is logged and skipped. Oracle triggers come from
  `DBMS_METADATA` with the schema prefix rewritten.
* Lock source tables runs `LOCK TABLE ... IN SHARE MODE` on Oracle sources
  and releases it at the end. SQLite has nothing to lock.
* The file target can write CSV: one `<table>.csv` per table in a folder,
  header row, formula-safe text cells. Existing files are only replaced when
  Drop target objects before create is on. Keys, indexes and triggers do not
  apply to CSV.

## Not done

XLSX file targets, views and other object kinds, binary values over 2000
bytes into Oracle, column types beyond a simple Oracle/SQLite mapping, and
the Oracle paths (constraint queries, trigger DDL, LOCK TABLE, keyset paging
on huge tables) have only been checked against SQLite and unit tests, not a
real Oracle server.
