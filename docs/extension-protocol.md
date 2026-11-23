# Extension protocol (version 1)

An extension is a separate program that Kilat starts and talks to over its
stdin and stdout. The host side is `crates/ora-core/src/extension_host.rs`.

## Transport
- UTF-8, one JSON object per line. No line break inside a message.
- Kilat sends one request at a time and waits for its answer.
- stdout carries messages only. Anything for people goes to stderr, which
  Kilat keeps (last 8 KB) for error messages and never parses.
- A line longer than 8 MiB, invalid JSON, an answer with the wrong `id`, or
  silence longer than the call timeout makes Kilat kill the program.

## Messages
Request (Kilat to extension):

    {"id": 2, "method": "query", "params": { ... }}

Answers (extension to Kilat), all carrying the request `id`:

    {"id": 2, "chunk": { ... }}                         zero or more, streaming
    {"id": 2, "result": { ... }}                        the end
    {"id": 2, "error": {"code": -1, "message": "..."}}  the end

`chunk` is used by methods that return many rows, so a large result never
needs to fit in one line. A method that does not stream must not send chunks.

## Handshake
The first request is always:

    {"id": 1, "method": "hello", "params": {"protocol": 1, "app": "0.1.51"}}

and the extension must answer, within a few seconds:

    {"id": 1, "result": {"protocol": 1, "id": "sqlite", "version": "1.0.0"}}

`protocol` must equal the host's, and `id` must be the id the extension is
installed under. Anything else is refused.

## Shutdown
Kilat sends `{"id":0,"method":"shutdown"}` and closes stdin. The extension
should exit; if it is still running a second later it is killed.

## Methods
The database methods mirror the `DbClient` trait in `ora-core`
(`connect`, `query`, `list_objects`, ...). Their parameters and results are
specified together with the SQLite pilot; this file only fixes the
transport and handshake.
