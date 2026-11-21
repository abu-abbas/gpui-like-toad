//! "Recordset Generator": split a table into ranges of an integer or date
//! field, so a big table is copied in separate slices.

use crate::dialect::DbKind;

use super::spec::MAX_RECORDSETS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    Integer,
    Date,
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD` to days; the date must exist.
pub fn parse_date(text: &str) -> Result<i64, String> {
    let bad = || format!("'{text}' is not a date (use YYYY-MM-DD).");
    let parts: Vec<&str> = text.trim().split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return Err(bad());
    }
    let nums: Vec<i64> = parts
        .iter()
        .map(|p| p.parse::<i64>().map_err(|_| bad()))
        .collect::<Result<_, _>>()?;
    let days = days_from_civil(nums[0], nums[1], nums[2]);
    if civil_from_days(days) != (nums[0], nums[1], nums[2]) {
        return Err(bad());
    }
    Ok(days)
}

pub fn format_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `count` conditions on `field` covering `start..=end` without gaps or
/// overlap. Fewer come back when the range has fewer values than `count`.
/// Values are checked here, so nothing typed ends up unquoted in SQL.
pub fn generate(
    source: DbKind,
    field: &str,
    kind: RangeKind,
    start: &str,
    end: &str,
    count: usize,
) -> Result<Vec<String>, String> {
    if field.trim().is_empty() {
        return Err("Choose the field to split on.".to_string());
    }
    if count == 0 || count > MAX_RECORDSETS {
        return Err(format!(
            "The number of recordsets must be 1 to {MAX_RECORDSETS}."
        ));
    }
    let f = source.dialect().quote_ident(field);
    let (lo, hi) = match kind {
        RangeKind::Integer => {
            let parse = |t: &str| {
                t.trim()
                    .parse::<i64>()
                    .map_err(|_| format!("'{t}' is not a whole number."))
            };
            (parse(start)?, parse(end)?)
        }
        RangeKind::Date => (parse_date(start)?, parse_date(end)?),
    };
    if hi < lo {
        return Err("The end value is before the start value.".to_string());
    }
    let total = i128::from(hi) - i128::from(lo) + 1;
    let size = (total + count as i128 - 1) / count as i128;
    let mut out = Vec::new();
    let mut from = i128::from(lo);
    while from <= i128::from(hi) {
        let next = (from + size).min(i128::from(hi) + 1);
        out.push(match kind {
            // `next` fits: it is at most hi + 1.
            RangeKind::Integer => format!("{f} >= {from} AND {f} < {next}"),
            RangeKind::Date => {
                let lit = |d: i128| match source {
                    DbKind::Oracle => format!("DATE '{}'", format_date(d as i64)),
                    DbKind::Sqlite => format!("'{}'", format_date(d as i64)),
                };
                format!("{f} >= {} AND {f} < {}", lit(from), lit(next))
            }
        });
        from = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_ranges_cover_without_overlap() {
        let c = generate(DbKind::Oracle, "ID", RangeKind::Integer, "1", "10", 3).unwrap();
        assert_eq!(
            c,
            [
                "\"ID\" >= 1 AND \"ID\" < 5",
                "\"ID\" >= 5 AND \"ID\" < 9",
                "\"ID\" >= 9 AND \"ID\" < 11"
            ]
        );
        let few = generate(DbKind::Sqlite, "n", RangeKind::Integer, "1", "2", 5).unwrap();
        assert_eq!(few.len(), 2);
    }

    #[test]
    fn date_ranges_per_dialect() {
        let o = generate(
            DbKind::Oracle,
            "D",
            RangeKind::Date,
            "2024-02-27",
            "2024-03-02",
            2,
        )
        .unwrap();
        assert_eq!(
            o,
            [
                "\"D\" >= DATE '2024-02-27' AND \"D\" < DATE '2024-03-01'",
                "\"D\" >= DATE '2024-03-01' AND \"D\" < DATE '2024-03-03'"
            ]
        );
        let s = generate(
            DbKind::Sqlite,
            "D",
            RangeKind::Date,
            "2024-01-01",
            "2024-01-01",
            3,
        )
        .unwrap();
        assert_eq!(s, ["\"D\" >= '2024-01-01' AND \"D\" < '2024-01-02'"]);
    }

    #[test]
    fn bad_input_is_refused() {
        let g = |s: &str, e: &str, k, n| generate(DbKind::Oracle, "ID", k, s, e, n);
        assert!(g("1", "x", RangeKind::Integer, 2).is_err());
        assert!(g("5", "1", RangeKind::Integer, 2).is_err());
        assert!(g("1", "5", RangeKind::Integer, 0).is_err());
        assert!(g("2023-02-30", "2023-03-01", RangeKind::Date, 2).is_err());
        assert!(g("1; DROP", "5", RangeKind::Integer, 2).is_err());
        assert!(generate(DbKind::Oracle, "", RangeKind::Integer, "1", "2", 1).is_err());
    }

    #[test]
    fn civil_dates_round_trip() {
        for text in ["1970-01-01", "2000-02-29", "2024-12-31", "1969-07-20"] {
            assert_eq!(format_date(parse_date(text).unwrap()), text);
        }
    }
}
