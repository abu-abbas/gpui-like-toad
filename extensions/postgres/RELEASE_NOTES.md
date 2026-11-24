# PostgreSQL extension release notes

## [1.0.1](https://github.com/abu-abbas/kilat/releases/tag/ext-postgres-1.0.1) · 2026-11-24

Minimum Kilat version `0.1.55`. The Explain analyze button appears in Kilat 0.1.63 and newer; older versions keep working without it.

- Adds EXPLAIN ANALYZE (`EXPLAIN (ANALYZE, BUFFERS)`) for the Explain analyze button in Kilat. The statement is run inside a transaction that is always rolled back, and your own open transaction is left untouched.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-postgres-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.55`.

- First signed PostgreSQL package for installation from Settings → Extensions.
- Connect to PostgreSQL, browse schemas and tables, and run SQL from Kilat.
