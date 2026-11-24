# PostgreSQL extension release notes

## 1.0.1 · Unreleased

Minimum Kilat version `0.1.55`.

- Adds EXPLAIN ANALYZE (`EXPLAIN (ANALYZE, BUFFERS)`) for the new Explain analyze button in Kilat. The statement is run inside a transaction that is always rolled back, and your own open transaction is left untouched. Needs a Kilat version with the button (released together with this extension); older Kilat versions simply do not show the button.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-postgres-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.55`.

- First signed PostgreSQL package for installation from Settings → Extensions.
- Connect to PostgreSQL, browse schemas and tables, and run SQL from Kilat.
