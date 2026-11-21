//! What a transfer does, as chosen in the wizard.

/// Rows copied per INSERT batch unless the user says otherwise.
pub const DEFAULT_BATCH: usize = 1000;
/// Upper bound of the batch size (the data tab's page cap).
pub const MAX_BATCH: usize = crate::MAX_PAGE_LIMIT;
/// Most recordsets one table may be split into.
pub const MAX_RECORDSETS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCase {
    Lower,
    Upper,
}

impl NameCase {
    pub fn apply(self, name: &str) -> String {
        match self {
            Self::Lower => name.to_lowercase(),
            Self::Upper => name.to_uppercase(),
        }
    }
}

/// The wizard's Option dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOptions {
    /// Create the target table when it does not exist.
    pub create_tables: bool,
    /// Also create the source table's indexes (after the rows are in).
    pub include_indexes: bool,
    /// Copy the rows.
    pub create_records: bool,
    /// One transaction per table: all its rows or none (unless Continue
    /// on error is on, which keeps the rows that did go in).
    pub use_transaction: bool,
    /// INSERT statements name their columns. Always on when the field
    /// mapping is not the table's own column order.
    pub complete_inserts: bool,
    /// A failing statement is logged and skipped instead of stopping.
    pub continue_on_error: bool,
    /// Drop an existing target table before creating it.
    pub drop_target: bool,
    /// `DELETE` the rows of an existing target table first.
    pub empty_target: bool,
    /// Convert table, column and index names of the target.
    pub convert_case: Option<NameCase>,
}

impl Default for TransferOptions {
    fn default() -> Self {
        TransferOptions {
            create_tables: true,
            include_indexes: true,
            create_records: true,
            use_transaction: true,
            complete_inserts: true,
            continue_on_error: false,
            // Destructive options start off, unlike Navicat's defaults.
            drop_target: false,
            empty_target: false,
            convert_case: None,
        }
    }
}

impl TransferOptions {
    /// Drops or empties target data: needs the confirmation dialog.
    pub fn destroys_target_data(&self) -> bool {
        (self.create_tables && self.drop_target) || (self.create_records && self.empty_target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMap {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fields {
    #[default]
    All,
    Custom(Vec<FieldMap>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rows {
    /// Every row, `batch` rows per INSERT batch.
    All { batch: usize },
    /// Only the rows of each condition (a `WHERE` expression on the source,
    /// e.g. from the Recordset Generator), one recordset after another.
    Custom {
        batch: usize,
        conditions: Vec<String>,
    },
}

impl Default for Rows {
    fn default() -> Self {
        Rows::All {
            batch: DEFAULT_BATCH,
        }
    }
}

impl Rows {
    pub fn batch(&self) -> usize {
        match self {
            Rows::All { batch } | Rows::Custom { batch, .. } => (*batch).clamp(1, MAX_BATCH),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSpec {
    pub source: String,
    /// `None`: same name as the source (Auto mode).
    pub target: Option<String>,
    pub fields: Fields,
    pub rows: Rows,
}

impl TableSpec {
    pub fn auto(source: impl Into<String>) -> Self {
        TableSpec {
            source: source.into(),
            target: None,
            fields: Fields::All,
            rows: Rows::default(),
        }
    }

    /// Target table name before case conversion.
    pub fn target_name(&self) -> &str {
        self.target.as_deref().unwrap_or(&self.source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSpec {
    pub source_schema: String,
    pub target_schema: String,
    pub tables: Vec<TableSpec>,
    pub options: TransferOptions,
}

/// A `WHERE` condition typed by the user must stay one expression: no
/// statement end, no comment, balanced parentheses and quotes.
pub fn check_condition(condition: &str) -> Result<(), String> {
    let text = condition.trim();
    if text.is_empty() {
        return Err("A recordset condition is empty.".to_string());
    }
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    for c in text.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            prev = c;
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            ';' => return Err("A recordset condition must not contain ';'.".to_string()),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return Err("A recordset condition has an unmatched ')'.".to_string());
                }
            }
            '-' if prev == '-' => {
                return Err("A recordset condition must not contain a comment.".to_string())
            }
            '*' if prev == '/' => {
                return Err("A recordset condition must not contain a comment.".to_string())
            }
            _ => {}
        }
        prev = c;
    }
    if quote.is_some() {
        return Err("A recordset condition has an unclosed quote.".to_string());
    }
    if depth != 0 {
        return Err("A recordset condition has an unclosed '('.".to_string());
    }
    Ok(())
}

impl TransferSpec {
    /// Check the choices before anything runs. Names are checked against
    /// the databases later (the runner reports unknown columns per table).
    pub fn validate(&self) -> Result<(), String> {
        if self.tables.is_empty() {
            return Err("Select at least one table.".to_string());
        }
        if self.source_schema.trim().is_empty() || self.target_schema.trim().is_empty() {
            return Err("Choose a source and a target schema.".to_string());
        }
        let o = &self.options;
        if !o.create_tables && !o.create_records {
            return Err("Nothing to do: turn on Create tables or Create records.".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for table in &self.tables {
            let target = match o.convert_case {
                Some(case) => case.apply(table.target_name()),
                None => table.target_name().to_string(),
            };
            if target.trim().is_empty() {
                return Err(format!("{}: the target name is empty.", table.source));
            }
            if !seen.insert(target.clone()) {
                return Err(format!("Two tables would be written to {target}."));
            }
            if let Fields::Custom(map) = &table.fields {
                if map.is_empty() {
                    return Err(format!("{}: pick at least one field.", table.source));
                }
                let mut names = std::collections::HashSet::new();
                for field in map {
                    if field.target.trim().is_empty() {
                        return Err(format!("{}: a target field name is empty.", table.source));
                    }
                    if !names.insert(field.target.to_lowercase()) {
                        return Err(format!(
                            "{}: target field {} appears twice.",
                            table.source, field.target
                        ));
                    }
                }
            }
            if let Rows::Custom { conditions, .. } = &table.rows {
                if conditions.is_empty() {
                    return Err(format!("{}: add at least one recordset.", table.source));
                }
                if conditions.len() > MAX_RECORDSETS {
                    return Err(format!(
                        "{}: more than {MAX_RECORDSETS} recordsets.",
                        table.source
                    ));
                }
                for condition in conditions {
                    check_condition(condition).map_err(|e| format!("{}: {e}", table.source))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tables: Vec<TableSpec>) -> TransferSpec {
        TransferSpec {
            source_schema: "A".into(),
            target_schema: "B".into(),
            tables,
            options: TransferOptions::default(),
        }
    }

    #[test]
    fn validation_catches_empty_and_duplicate_choices() {
        assert!(spec(vec![]).validate().is_err());
        let mut a = TableSpec::auto("T");
        let b = TableSpec::auto("t");
        assert!(spec(vec![a.clone(), b.clone()]).validate().is_ok());
        let mut s = spec(vec![a.clone(), b]);
        s.options.convert_case = Some(NameCase::Upper);
        assert!(s.validate().unwrap_err().contains("Two tables"));
        a.fields = Fields::Custom(vec![]);
        assert!(spec(vec![a]).validate().is_err());
        let mut off = spec(vec![TableSpec::auto("T")]);
        off.options.create_tables = false;
        off.options.create_records = false;
        assert!(off.validate().is_err());
    }

    #[test]
    fn conditions_stay_single_expressions() {
        assert!(check_condition("\"ID\" >= 1 AND \"ID\" < 100").is_ok());
        assert!(check_condition("NAME = 'a;b'").is_ok());
        for bad in [
            "",
            "1=1; DROP TABLE x",
            "a=1 -- x",
            "a=1 /* x",
            "(a=1",
            "a=1)",
            "a='x",
        ] {
            assert!(check_condition(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn destructive_options_need_their_parent_option() {
        let mut o = TransferOptions::default();
        assert!(!o.destroys_target_data());
        o.drop_target = true;
        assert!(o.destroys_target_data());
        o.create_tables = false;
        assert!(!o.destroys_target_data());
        o.empty_target = true;
        assert!(o.destroys_target_data());
    }
}
