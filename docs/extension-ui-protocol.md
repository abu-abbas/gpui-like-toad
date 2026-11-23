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

## Validation

The host validates an entire reply before applying it. Each free-form string
is limited to 64 KiB, the full message shares the transport's 8 MiB limit,
and nesting is at most 32 view nodes deep. The 5,000-entry budget is shared
by all slots in a reply and includes rows, table cells, choices, tabs,
menu items and effects. Interactive IDs are unique within a slot. Item IDs
are unique within their component. Unknown component and event types are
errors.

Links require HTTP or HTTPS with a host and no embedded credentials.
Clipboard, external URL and confirmation effects require a response to a
user event. The host still verifies which action triggered the effect and
applies Read Only and production checks before forwarding writes. The
extension also enforces permissions at the service boundary. Polling uses
a minimum interval of one second. A reply without a polling interval stops polling.

A protocol violation closes the extension session. The renderer integration
will surface Connection lost and offer Reconnect. The transport, renderer,
manifest contributions and tool lifecycle are separate layers.
