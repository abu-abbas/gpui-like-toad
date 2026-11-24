# SQL Server extension release notes

## [1.0.1](https://github.com/abu-abbas/kilat/releases/tag/ext-mssql-1.0.1) · 2026-11-25

Minimum Kilat version `0.1.64`.

- No change in what the extension does. The extension list now says it needs Kilat 0.1.64 or newer (1.0.0 was listed as 0.1.63), so Kilat 0.1.63 no longer offers to install it.

## [1.0.0](https://github.com/abu-abbas/kilat/releases/tag/ext-mssql-1.0.0) · 2026-11-25

Minimum Kilat version `0.1.64`.

- First signed SQL Server package for installation from Settings → Extensions.
- Connect with a SQL Server login (user and password). The connection is encrypted and the server's certificate is checked; tick "Trust server certificate" in the connection form only for a server you know, such as a development server with a self-signed certificate.
- The explorer's dropdown lists the databases of the server and shows the chosen database's objects grouped by schema. Each data tab stays bound to the database it was opened in, so `dev.dbo.users` and `staging.dbo.users` can be open side by side, with their own pages, row counts, structure, indexes and script.
- Tables are paged in primary key order (a unique index when there is no primary key). A table with neither shows a note that rows with equal sort values may move between pages.
- Data, Structure, Index and DDL views, filters, paging, row count, CSV/XLSX export and the SQL editor work. Editing rows, importing, Data Transfer and Explain plan are not available yet.
- Stop waiting ends the connection instead of cancelling the query on the server (SQL Server stops the query, but the driver cannot read its acknowledgement, and the next query on that session would return no rows).
