//! Headless core of kilat.
//!
//! Everything in this crate is UI-free on purpose: the database client trait,
//! the result model, CSV export, SQL text helpers, and an in-memory demo
//! backend that lets the GUI run end-to-end without an Oracle database.
//!
//! Real Oracle access lives in `ora-driver` (thin driver, no Oracle Client
//! install). The GUI crate only talks to [`client::DbClient`], so the driver
//! can be swapped without touching any UI code.

pub mod binds;
pub mod client;
pub mod completion;
pub mod copy;
pub mod credentials;
pub mod ddl;
pub mod demo;
pub mod dialect;
pub mod edit;
pub mod export;
pub mod extension_client;
pub mod extension_host;
pub mod extension_index;
pub mod extension_serve;
pub mod extension_ui;
pub mod extension_wire;
pub mod fetch;
pub mod filter;
pub mod import;
pub mod model;
pub mod oauth;
pub mod pretty;
pub mod recycle;
pub mod redact;
pub mod sql;
pub mod store;
pub mod timing;
pub mod transfer;
pub mod update;

pub use client::{DbClient, DbError, DbInfo};
pub use dialect::{Capabilities, DbKind, Dialect};
pub use fetch::{FetchLimits, RowCollector};
pub use filter::{
    BoundQuery, FilterCondition, FilterJoin, FilterOp, TableFilter, TablePage, TableTarget,
    MAX_PAGE_LIMIT, ROWID_COLUMN,
};
pub use model::{
    compose_easy_connect, default_profile_name, parse_easy_connect, ClippedValue, ClobPreview,
    ColumnDef, ColumnInfo, ConnectionAccess, ConnectionEnvironment, ConnectionProfile,
    ConstraintDef, ConstraintKind, IndexDef, LenUnit, ObjectKind, PlanNode, QueryResult,
    SchemaObject, Value,
};

/// Default cap for rows fetched into the grid. Queries that hit the cap set
/// [`QueryResult::truncated`] so the UI can say so instead of silently
/// showing a partial result.
pub const DEFAULT_MAX_ROWS: usize = 1000;

/// Row cap for "Export Data" on a whole table. Higher than the grid cap
/// because nothing is rendered; the UI reports when the cap was hit.
pub const EXPORT_MAX_ROWS: usize = 100_000;
