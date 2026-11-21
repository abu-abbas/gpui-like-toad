//! Data Transfer: copy tables from one connection/schema to another (or to
//! an SQL script file), like Navicat's Data Transfer wizard.
//!
//! This module is UI-free and database-free: it works on the [`TransferIo`]
//! trait, which the app implements on top of the connection workers and
//! tests implement on top of real in-memory drivers.
//!
//! * [`spec`]: what the user chose (tables, field mapping, row ranges,
//!   options) and its validation.
//! * [`sqlgen`]: every statement. Identifiers go through the dialect's
//!   quoting; values become typed literals, never raw text.
//! * [`ranges`]: the "Recordset Generator" (integer and date ranges).
//! * [`run`]: the runner: DDL, batched copy, commit/rollback, Stop.
//! * [`profile`]: Save Profile / Load Profile.
//!
//! Row contents and passwords are never written to the event log: events
//! carry table names, counts and database error text only.

pub mod profile;
pub mod ranges;
pub mod run;
pub mod spec;
pub mod sqlgen;

pub use profile::{TransferProfile, TransferTarget};
pub use run::{
    run_transfer, ClientIo, Event, LogLevel, ScriptIo, TableOutcome, TransferIo, TransferSummary,
};
pub use spec::{FieldMap, Fields, NameCase, Rows, TableSpec, TransferOptions, TransferSpec};
