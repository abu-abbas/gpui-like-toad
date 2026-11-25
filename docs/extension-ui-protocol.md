# Declarative extension UI protocol v1

The host owns the sidebar, explorer and main column, their sizes, theme and
keyboard navigation. An extension describes the contents with native
components. The UI protocol uses the existing extension transport and has
its own version. The wire types and their validator live in
`crates/ora-core/src/extension_ui.rs`.

## Requests and replies

- `ui.open` takes a `slot` of `sidebar`, `explorer` or `main`.
- `ui.event` takes an event with `revision`, `slot`, `node` and a tagged
  `event`. The revision identifies the view the event came from.
- `ui.tick` takes the current revision. The host drives polling while the
  view is active and serializes it with user requests.

Replies are view updates. An event can update several slots together.
An unlisted slot stays as it was. A listed slot with a null tree clears it.
The revision is scoped to one extension session. An update that changes the
view advances the revision. The renderer must ignore stale responses and
keep local draft input, focus and scroll keyed by stable component IDs.

```json
{
  "version": 1,
  "revision": 12,
  "slots": [
    {"slot": "main", "tree": {"type": "text", "style": "heading", "text": "Welcome"}}
  ],
  "poll_ms": 5000
}
```

An input event example is
`{"revision":12,"slot":"explorer","node":"search","event":{"type":"change","value":"Kilat"}}`.
A list selection supplies its row ID in `event.item`. Tabs and menu actions
likewise identify their item by ID. List `more` requests another page. The
extension supplies the resulting view, including any rows it wants to keep
on screen. The host must keep the view bounded.

## Components added in Kilat 0.1.65

These are part of protocol v1 but an older Kilat rejects them as unknown
types, so an extension that sends them sets `min_app` to `0.1.65` or later.

- `banner`: `tone`, `icon`, `title`, `text` and `children` (badges, progress,
  buttons) on the right, drawn as a tinted strip.
- `progress`: `value` from 0 to 1, `tone`, `label`, `trailing`.
- `card`: `icon`, `title`, `badge`, `badge_tone`, `subtitle` and an optional
  `child`, drawn as a bordered box with a header line.
- `diff`: like `table`, but each cell is `{"text", "old"?, "removed"?}`. With
  `old` the old value is struck out next to the new one; `removed` strikes
  the value out.
- `button` takes an optional `icon` and the `success` variant.

Icon names: `flask`, `lock`, `undo`, `check`, `zap`, `table`, `x`, `alert`,
`copy`, plus the list icons (`search`, `trash`, `refresh`, `key`, `type-*`).

## Added in Kilat 0.1.68

An older Kilat rejects these, so an extension that sends them sets `min_app`
to `0.1.68` or later.

- The `save` effect: `{"type": "save", "name": "undo_emp.sql", "parts": [...]}`.
  Kilat asks where to save, suggesting `name`, and writes the parts one after
  another. `name` must be a plain file name (no folders, no leading dot, no
  `/ \ : * ? " < > |` or control characters). Parts let a file be larger than
  one 64 KiB string; the whole reply still has to fit the 8 MiB transport
  limit. Like the clipboard effect it requires a user event.
- The `download` icon.
- `row` children are centred vertically.

## Validation

The host validates an entire reply before applying it. Each free-form string
is limited to 64 KiB, the full message shares the transport's 8 MiB limit,
and nesting is at most 32 view nodes deep. The 5,000-entry budget is shared
by all slots in a reply and includes rows, table cells, choices, tabs,
menu items and effects. Interactive IDs are unique within a slot. Item IDs
are unique within their component. Unknown component and event types are
errors.

Links require HTTP or HTTPS with a host and no embedded credentials.
Clipboard, save, external URL and confirmation effects require a response
to a user event. The host still verifies which action triggered the effect and
applies Read Only and production checks before forwarding writes. The
extension also enforces permissions at the service boundary. Polling uses
a minimum interval of one second. A reply without a polling interval stops polling.

A protocol violation closes the extension session. The renderer integration
will surface Connection lost and offer Reconnect. The transport, renderer,
manifest contributions and tool lifecycle are separate layers.

## MailHog pilot

Kilat uses the declarative Inbox when the signed MailHog package is installed
and enabled. Existing tool profiles keep their IDs and keychain entries.
The Workbench supplies their existing connection settings to the extension,
which publishes the sidebar status, Explorer and Main view together.
The bundled Inbox stays available for existing profiles without the pilot
package. The pilot currently uses the built-in profile editor.

The pilot supports inbox search, paging, message tabs, copying the subject
and deleting a message with host confirmation. Read-only is also enforced
by the MailHog service. The pilot caps the retained rows and message text.
When a local server is requested, the extension reuses a listening instance
or starts and owns a child process. The host exposes its log in Settings and
cleans up recorded leftovers on a subsequent Kilat startup.
