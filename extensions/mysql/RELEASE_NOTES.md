# MySQL extension release notes

## 1.0.1 · Unreleased

Minimum Kilat version `0.1.55`.

- Tables without a primary key can now be edited and have rows deleted in the data tab. The row is found again by every column that compares exactly; if several rows are identical, one of them changes. After editing a value, refresh before editing the same row again.
- Adds EXPLAIN ANALYZE (`ANALYZE` on MariaDB) for the new Explain analyze button in Kilat. The statement is run inside a transaction that is always rolled back, and your own open transaction is left untouched. Tables on a non-transactional engine (MyISAM) keep any changes.
- Checked against MariaDB 10.11 as well as MySQL 8.0.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-mysql-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.55`.

- First signed MySQL package for installation from Settings → Extensions.
- Connect to MySQL and MariaDB, browse tables, and run SQL from Kilat.
