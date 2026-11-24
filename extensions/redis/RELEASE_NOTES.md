# Redis extension release notes

## 1.0.0 · Unreleased

Minimum Kilat version `0.1.62`.

- First signed Redis package for installation from Settings → Extensions.
- Browse keys with SCAN by database, type and pattern, and read strings (JSON is indented), hashes, lists, sets, sorted sets and streams.
- Run commands in the Console. Dangerous commands stay blocked unless the connection allows them, and PROD connections only run read commands and cannot delete keys.
- Connect to an existing Redis server or let the extension start a local `redis-server`; its output appears in Settings → Logs → Developer Tools.
