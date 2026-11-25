# OpenSSL backend for MySQL

Source is the published native-tls 0.2.18 crate. This copy retains the upstream OpenSSL implementation and uses it on every target. The MySQL extension builds it with vendored OpenSSL so its binary does not depend on a separately installed OpenSSL library.

MySQL 5.7 servers can require TLS 1.2 with DHE-RSA. Rustls does not offer that cipher suite with its built-in providers, and macOS Secure Transport excludes it from its default configuration. The mysql crate uses native-tls internally and does not expose a custom connector. This patch keeps the upstream native-tls API and gives that client the same TLS backend on Linux, macOS, and Windows.

When updating native-tls, reapply the backend selection in src/lib.rs and the unconditional OpenSSL dependencies in Cargo.toml.

Kilat's SQL Server driver also uses platform trust roots and scopes an explicit
legacy TLS opt-in to connector creation on its current-thread runtime. The
normal OpenSSL security level and TLS 1.2 minimum apply outside that scope.
