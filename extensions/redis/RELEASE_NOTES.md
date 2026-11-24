# Redis extension release notes

## 1.0.1 · Unreleased

Minimum Kilat version `0.1.65`.

- The local `redis-server` data folder (`dump.rdb`) and log are now kept per connection id instead of per connection name, so renaming a Redis connection no longer starts an empty database. Data: `server-data/redis-extension/<id>/`, log: `server-logs/developer-tools/redis/<id>/redis.log`, like MailHog.
- The old folder and log (named after the connection) move to the new place the next time the server starts.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-redis-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.63`.

- First signed Redis package for installation from Settings → Extensions.
- Browse keys with SCAN by database, type and pattern, and read strings (JSON is indented), hashes, lists, sets, sorted sets and streams.
- Run commands in the Console. Dangerous commands stay blocked unless the connection allows them, and PROD connections only run read commands and cannot delete keys.
- Connect to an existing Redis server or let the extension start a local `redis-server`; its output appears in Settings → Logs → Developer Tools.
