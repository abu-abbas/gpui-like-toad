# MailHog extension release notes

## 1.0.2 · Unreleased

Minimum Kilat version `0.1.60`.

- Recreate the local MailHog log on the next line of output if its file or folder was removed while the server was running.

## [1.0.1](https://github.com/abu-abbas/kilat/releases/tag/ext-mailhog-1.0.1) · 2026-11-24

Minimum Kilat version `0.1.60`.

- Local MailHog logs are saved under `server-logs/developer-tools/mailhog/<instance-id>/` and appear in Settings → Logs → Developer Tools.
- Existing logs and their rotated copies move when the local server next starts. If both locations already have logs, Settings still lists both.
- Every new line starts with a local timestamp and UTC offset.
- Restore Start, Restart and Stop in the connection's context menu. Stopping a managed server keeps its view open with a Start server button in the Inbox column.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-mailhog-1.0.0) · 2026-11-24

Minimum Kilat version `0.1.55`.

- First signed MailHog package for installation from Settings → Extensions.
- Browse and search messages, open Text, HTML, Headers and Raw tabs, and copy message subjects.
- Connect to an existing MailHog server or let the extension manage a local server. Read-only profiles block message deletion.
