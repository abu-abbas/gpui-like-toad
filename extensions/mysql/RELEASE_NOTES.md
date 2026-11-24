# MySQL extension release notes

## [1.0.2](https://github.com/abu-abbas/kilat/releases/tag/ext-mysql-1.0.2) · 2026-11-24

Minimum Kilat version `0.1.55`. The Explain analyze button appears in Kilat 0.1.63 and newer; older versions keep working without it.

- Adds EXPLAIN ANALYZE (`ANALYZE` on MariaDB) for the Explain analyze button in Kilat. The statement is run inside a transaction that is always rolled back, and your own open transaction is left untouched. Tables on a non-transactional engine (MyISAM) keep any changes.

## [1.0.1](https://github.com/abu-abbas/kilat/releases/tag/ext-mysql-1.0.1) · 2026-11-24

Minimum Kilat version `0.1.55`.

- Tables without a primary key can now be edited and have rows deleted in the data tab. The row is found again by every column that compares exactly; if several rows are identical, one of them changes. After editing a value, refresh before editing the same row again.
- Checked against MariaDB 10.11 as well as MySQL 8.0.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-mysql-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.55`.

- First signed MySQL package for installation from Settings → Extensions.
- Connect to MySQL and MariaDB, browse tables, and run SQL from Kilat.
