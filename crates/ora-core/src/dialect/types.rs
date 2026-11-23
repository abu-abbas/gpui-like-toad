//! Column types of every engine, through one neutral list.
//!
//! A transfer reads the source type with the source dialect's
//! [`crate::Dialect::canonical_type`] and writes the target type with the
//! target dialect's [`crate::Dialect::native_type`]. A new engine adds two
//! functions instead of a mapping to every other engine.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonType {
    /// Whole numbers; `digits` is the declared precision when known.
    Integer {
        digits: Option<u32>,
    },
    /// Exact decimals; `None` for unconstrained.
    Decimal {
        precision: Option<u32>,
        scale: Option<u32>,
    },
    /// Binary floating point.
    Double,
    /// Text up to `len` characters, or unbounded for `None`.
    Text {
        len: Option<u32>,
    },
    /// Large text.
    Clob,
    Binary,
    Blob,
    Date,
    /// Date and time without zone.
    Timestamp,
    TimestampTz,
    Time,
    Boolean,
    Json,
}
