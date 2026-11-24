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

## Database methods
The database methods mirror the `DbClient` trait in `ora-core`. Both sides
are in `ora-core`: `extension_client.rs` (Kilat) and `extension_serve.rs`
(the extension); the JSON shapes are in `extension_wire.rs`. `params` is
always an object. A method an extension cannot do answers with an error.

| method | params | result |
|---|---|---|
| `connect` | `profile`, `password` | `{user, database, version}` |
| `disconnect` | none | `{}` |
| `query` | `sql`, `binds` (text list), `limits` | rows in chunks, then the result head |
| `query_table` | `owner`, `table`, `filter`, `page` | like `query` |
| `execute` | `sql`, `binds` | `{affected}` |
| `commit`, `rollback` | none | `{}` |
| `list_schemas` | none | list of names |
| `list_objects` | `owner` | list of `{owner,name,kind,parent,detail}` |
| `object_ddl` | `owner`, `name`, `kind`, `show_storage` | text |
| `describe_table` | `owner`, `name` | list of `{name,type,nullable,default}` |
| `list_indexes`, `list_constraints`, `trigger_scripts` | `owner`, `table` | lists |
| `explain_plan`, `explain_plan_nodes` | `sql` | text lines / plan nodes |

Row results: every `chunk` is `{"rows": [row, ...]}` (a row is a list of
cells, see below), at most about 512 KB each. The closing `result` has the
other fields of a query result (`columns`, `truncated`, `byte_limited`,
`elapsed_ms`, `fetch_ms`) and an empty `rows`. Kilat refuses more rows than
the `limits` it sent (`max_rows`, `max_cell_bytes`, `max_result_bytes`,
`read_lobs`) and kills the program if it sends them.

Cells: `null`, a string (text), an integer (up to 2^53), or a single-key
object for the rest: `{"i":"<decimal>"}`, `{"f":<n>|"NaN"|...}`,
`{"x":"<hex>"}` bytes, `{"c":{"p":<cell>,"n":<len>,"u":"bytes"|"chars"}}`
clipped.

Limits for Kilat: the handshake must finish within 10 seconds, other calls
may be silent for up to 10 minutes. A call that times out kills the program;
the next `connect` starts a new one.

## MailHog methods
An extension of kind `tool` has its own methods. The MailHog extension
(`crates/ext-mailhog`, id `mailhog`) wraps the MailHog HTTP API client of
`crates/devtools`; the JSON of messages and targets is in
`devtools/src/mailhog_wire.rs`, used by both sides. Kilat keeps drawing the
Inbox; the extension only answers with data.

| method | params | result |
|---|---|---|
| `connect` | `target` `{host, port, https, username, password}`, optional `start_server`, `server_command`, `instance_id`, `read_only` | `{total, managed_server}`. Fails when the MailHog API does not answer. |
| `disconnect` | none | `{}`. Stops a local server started by this extension process. |
| `server.stop` | none | Stops a server owned by the extension and returns the declarative stopped view. |
| `list` | `start` (messages to skip, newest first) | messages in chunks, then `{total, count}` |
| `search` | `kind` (`containing`, `from`, `to`), `query`, `start` | like `list` |
| `delete` | `id` | `{}` |
| `delete_all` | none | `{}` |

`managed_server` is true only when this extension process started the
server. Older extensions omit it. Kilat enables Stop and Restart when
the field is true. The stopped view declares a `start-server` button in the
Explorer slot. Kilat routes its click to a new connection using the saved
profile, and the extension starts the server as part of `connect`.

Every `chunk` of `list` and `search` is `{"messages": [message, ...]}`, at
most about 512 KB each. A message has `id`, `from`, `to`, `cc`, `subject`,
`date`, `created`, `size`, `headers` (list of `[name, value]`), `text`,
`html` (both may be null), `attachments` (`{name, content_type, size}`)
and `raw`. A message over 2 MB is sent without `raw` and with `text` and
`html` cut at 512 KB. Any method before `connect` (or after `disconnect`)
answers `not connected`.
