# Extension UI (design, not built yet)

Status: decided in review (2026-11-23), not built yet. Nothing in this document exists in the code.

## Why
Today SQLite is an extension (a separate program, `docs/extension-protocol.md`)
but Redis and MailHog are not: their screens (about 3,800 lines in
`workbench/tools*.rs`) live in the app, so every new tool would grow the app.
An extension can only be a real package if it can also bring its screens.

## Idea
Kilat owns the window: the three columns (sidebar, explorer, detail),
resizing, theme, fonts, focus and shortcuts. An extension fills the
columns by sending a **view tree**: JSON that names components from a fixed
set. Kilat draws it. The user's actions come back to the extension as
events, and the extension answers with the next tree. The extension never
runs code in Kilat and never draws pixels.

    Kilat                                   extension (separate program)
      |  ui.open {slot: "explorer"}  ---->  |
      |  <----  {tree: {...}}               |  builds the tree from its own state
      |  (draws it)                         |
      |  ui.event {node, event, value} -->  |  e.g. the user typed in the search box
      |  <----  {tree: {...}, effects: []}  |  fetches data, returns the new tree

It is the same transport as the database methods: one request at a time over
stdin and stdout, signed download, `hello` handshake, kill on violation.

## Slots
| slot | where | filled by the extension |
|---|---|---|
| `sidebar` | one entry (icon, name, status dot, context menu): under Connections for a `database` extension, under "Developer Tools" for a `tool` (that heading shows only while a tool is installed) | `ui.open slot=sidebar` |
| `explorer` | column 2 while the entry is active | `ui.open slot=explorer` |
| `main` | column 3 | `ui.open slot=main` |
| `form` | the "new / edit connection" dialog | the manifest (static, see below) |

Width, resize handles, the empty state and the tab strip stay with Kilat.

## Components (version 1)
Layout: `column`, `row`, `spacer`, `divider`, `scroll`.
Text: `text` (`style`: body, caption, mono, heading; `tone`: normal, muted,
danger, success, warning; `selectable`), `code` (`language`, read-only,
copy button), `badge`, `icon` (a name from Kilat's icon set), `link`
(http and https only; Kilat opens the browser).
Input: `button` (`variant`, `disabled`, optional `confirm` text that makes
Kilat ask first), `input` (`placeholder`, `value`, debounced `change`,
`submit`), `select` (`options`), `checkbox`, `menu` (a dropdown of actions).
Structure: `tabs` (`items`, `active`, one child per tab), `list`, `table`,
`kv` (key and value rows), `empty` (message in the middle).

`list` is the one that carries data. It is virtual: the extension sends one
page of rows, each with `id`, `title`, `subtitle`, `trailing`, `icon`,
`selected`, and a `more` flag. When the user scrolls to the end Kilat sends
the event `more`. A tree is therefore never large.

Icons: `icon` and every `icon` field name an icon of Kilat's built-in
interface set (search, trash, refresh, arrows, mail, key, ...). The extension's
own identity icon (the MailHog or Redis logo) is not in Kilat: it is a small
SVG (at most about 16 KB) in the extension's manifest entry in the signed
index, which Kilat checks for size and shape before drawing. A new extension
therefore never needs a new Kilat release for its logo.

Colors are tokens (`primary`, `danger`, `muted`, ...), never raw values, so
themes keep working. There is no custom drawing and no HTML. `html` mail is
shown as text, as today.

## Events
Every node has an `id` (stable between trees, so Kilat keeps scroll position
and focus). Events: `click`, `change` (text or choice, debounced 250 ms),
`submit`, `select` (a row), `more` (end of list), `tab`.
The reply to an event is `{ "tree": ..., "effects": [...] }`; `tree` may be
omitted when nothing changed. Effects are small and fixed: `toast`,
`copy` (to the clipboard), `open_url`, `confirm` (asks, and sends the answer
as an event). Typed text lives in Kilat so typing never waits for the
extension.

Changes that happen without a click (new mail arrives) use polling: a tree
may carry `poll_ms` (at least 1000), and Kilat then sends `ui.tick`. The
extension can answer with a new tree. This keeps the protocol one request at
a time, as it is today.

## Manifest
`extensions/<id>/extension.json` gets a `contributes` block:

    "contributes": {
      "sidebar": {"title": "MailHog"},
      "form": {"fields": [
        {"key": "host", "label": "Host", "type": "text", "default": "localhost"},
        {"key": "port", "label": "Port", "type": "number", "default": 8025},
        {"key": "https", "label": "Use HTTPS", "type": "bool"},
        {"key": "username", "label": "User", "type": "text", "optional": true},
        {"key": "password", "label": "Password", "type": "secret", "optional": true}
      ]}
    }

Kilat draws the form, stores the instance, keeps `secret` fields in the
keychain (as today) and sends the values to the extension in `connect`.

## Limits (checked by Kilat, violation kills the program)
At most 5,000 nodes per tree, depth 32, 64 KB per string, 8 MiB per message
(already the transport limit). Unknown component or event names are errors,
not ignored. A node of a newer component version is refused with a message
that names the extension and suggests updating Kilat (`min_app` in the
manifest, as for databases).

## Example: MailHog
Explorer (column 2):

    {"type":"column","children":[
      {"type":"select","id":"kind","value":"containing",
       "options":[["containing","All mail"],["from","From"],["to","To"]]},
      {"type":"input","id":"q","placeholder":"Search","value":""},
      {"type":"list","id":"mails","more":true,"rows":[
        {"id":"a1","title":"Reset your password","subtitle":"Acme App","trailing":"12:31"}]}
    ]}

Detail (column 3), after the row `a1` was selected:

    {"type":"column","children":[
      {"type":"heading","text":"Reset your password"},
      {"type":"kv","rows":[["From","Acme App <no-reply@acme.test>"],["To","dewi@example.com"]]},
      {"type":"tabs","id":"view","active":"text","items":[
        {"id":"text","label":"Text","child":{"type":"text","style":"body","selectable":true,"text":"Hi Dewi..."}},
        {"id":"headers","label":"Headers","child":{"type":"kv","rows":[]}},
        {"id":"raw","label":"Raw","child":{"type":"code","language":"text","text":"From: ..."}}]}
    ]}

Redis maps the same way: explorer = `select` (database), `select` (type),
`input` (SCAN pattern), `list` of keys; detail = `tabs` of Value (`code`
or `table`) and Console (`input` plus `code` for the log).

## What stays in Kilat
The window, columns, tabs, theme, shortcuts, the keychain, Read Only and
PROD guards (an extension declares whether an action changes data with
`"writes": true` on a `button`; Kilat refuses it on a Read Only connection
and asks for confirmation on PROD), errors.log, the updater and signed
downloads.

## Plan (separate PRs, each reviewable on its own)
1. Protocol and types: `extension_ui.rs` in `ora-core` (tree, events,
   validation and limits, JSON), docs, tests. No drawing.
2. Renderer in the app: draws a tree into a column, sends events, a fake
   extension in tests, a screenshot check on three themes.
3. Manifest `contributes`, the sidebar entry, the generated connection
   form, start and stop of the extension per instance.
4. MailHog on this protocol (`kilat-ext-mailhog` already wraps the client;
   its methods become `ui.*`), release workflow option, `min_app`. The built-in
   Inbox stays until the pilot is stable, then is removed like SQLite.
5. Redis the same way, then the Developer Tools code leaves the app.

## Decisions (John, 2026-11-23)
1. **Local server start**: the extension does it itself (option A). It
   starts the server on `connect` when the instance asks for it, stops it on
   `shutdown` and when stdin closes, and reuses a server that already listens
   on the port. The code stays in `devtools::server` and is linked into both
   extension programs. Settings > Logs reads developer tool logs under
   `server-logs/developer-tools/`. MailHog uses `mailhog/<instance-id>/mailhog.log`
   inside that folder. The `running.pids` journal tracks leftovers for the next
   app start. If the extension is killed, its server may outlive it until then;
   a new `connect` reuses it.
2. **Icons**: built-in interface set plus an SVG identity icon in the signed
   index entry (see Components).
3. **Sidebar**: by kind, `database` under Connections, `tool` under
   Developer Tools.

## When an extension crashes
Kilat is a separate process and is not affected. The closed pipe or the
timeout shows "Connection lost" with a Reconnect button, with the tail of
stderr in the message and in `errors.log`. There is no automatic restart:
only Reconnect starts a new process.
