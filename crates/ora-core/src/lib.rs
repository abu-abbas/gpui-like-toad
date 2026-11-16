//! Headless core of gpui-like-toad.
//!
//! Everything in this crate is UI-free on purpose: the database client trait,
//! the result model, CSV export, SQL text helpers, and an in-memory demo
//! backend that lets the GUI run end-to-end without an Oracle database.
//!
//! Real Oracle access lives in `ora-driver` (thin driver, no Oracle Client
//! install). The GUI crate only talks to [`client::DbClient`], so the driver
//! can be swapped without touching any UI code.

pub mod client;
pub mod demo;
pub mod export;
pub mod model;
pub mod sql;
pub mod store;

pub use client::{DbClient, DbError, DbInfo};
pub use model::{
    ColumnDef, ColumnInfo, ConnectionProfile, ObjectKind, PlanNode, QueryResult, SchemaObject,
    Value,
};

/// Default cap for rows fetched into the grid. Queries that hit the cap set
/// [`QueryResult::truncated`] so the UI can say so instead of silently
/// showing a partial result.
pub const DEFAULT_MAX_ROWS: usize = 1000;
