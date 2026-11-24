# SQL Server extension release notes

## 1.0.0 · Unreleased

Minimum Kilat version `0.1.64`.

- First signed SQL Server package for installation from Settings → Extensions.
- Connect with a SQL Server login (user and password). The connection is encrypted and the server's certificate is checked; tick "Trust server certificate" in the connection form only for a server you know, such as a development server with a self-signed certificate.
- The explorer's dropdown lists the databases of the server and shows the chosen database's objects grouped by schema. Each data tab stays bound to the database it was opened in, so `dev.dbo.users` and `staging.dbo.users` can be open side by side, with their own pages, row counts, structure, indexes and script.
- Tables are paged in primary key order (a unique index when there is no primary key). A table with neither shows a note that rows with equal sort values may move between pages.
- Data, Structure, Index and DDL views, filters, paging, row count, CSV/XLSX export and the SQL editor work. Editing rows, importing, Data Transfer and Explain plan are not available yet.
- Stop waiting ends the connection instead of cancelling the query on the server (SQL Server stops the query, but the driver cannot read its acknowledgement, and the next query on that session would return no rows).
