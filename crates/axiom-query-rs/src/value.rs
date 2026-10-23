//! Convert JSON domain values into typed SQL values.

use std::str::FromStr;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sea_orm::prelude::{Decimal, Uuid};
use sea_orm::sea_query::{ColumnType, Value};
use serde_json::Value as JsonValue;

use crate::error::{ErrorCode, QueryError, Result};
use crate::parser::json_type_name;
use crate::schema::ColumnInfo;

const DATETIME_FORMATS: [&str; 4] = [
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M",
];

/// Convert `value` to the SQL type of `column`.
///
/// Strings are parsed into dates, datetimes, decimals and UUIDs; a value that
/// cannot represent the column's type is rejected before reaching the database.
pub(crate) fn column_value(
    column: &ColumnInfo,
    field_path: &str,
    value: &JsonValue,
) -> Result<Value> {
    let mismatch = || {
        QueryError::new(
            ErrorCode::InvalidFilterValue,
            format!(
                "Value {value} is not valid for field '{field_path}' ({:?})",
                column.column_type
            ),
        )
    };

    let converted = match (&column.column_type, value) {
        (
            ColumnType::TinyInteger
            | ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::TinyUnsigned
            | ColumnType::SmallUnsigned
            | ColumnType::Unsigned
            | ColumnType::BigUnsigned,
            JsonValue::String(s),
        ) => s.trim().parse::<i64>().ok().map(Value::from),
        (ColumnType::Float | ColumnType::Double, JsonValue::String(s)) => {
            s.trim().parse::<f64>().ok().map(Value::from)
        }
        (ColumnType::Decimal(_) | ColumnType::Money(_), JsonValue::Number(n)) => {
            Decimal::from_str(&n.to_string())
                .or_else(|_| Decimal::from_scientific(&n.to_string()))
                .ok()
                .map(Value::from)
        }
        (ColumnType::Decimal(_) | ColumnType::Money(_), JsonValue::String(s)) => {
            Decimal::from_str(s.trim()).ok().map(Value::from)
        }
        (ColumnType::Date, JsonValue::String(s)) => parse_date(s).map(Value::from),
        (ColumnType::DateTime | ColumnType::Timestamp, JsonValue::String(s)) => {
            parse_datetime(s).map(Value::from)
        }
        (ColumnType::TimestampWithTimeZone, JsonValue::String(s)) => {
            parse_datetime_tz(s).map(Value::from)
        }
        (ColumnType::Time, JsonValue::String(s)) => parse_time(s).map(Value::from),
        (ColumnType::Uuid, JsonValue::String(s)) => Uuid::parse_str(s.trim()).ok().map(Value::from),
        (ColumnType::Boolean, JsonValue::Number(n)) => match n.as_i64() {
            Some(0) => Some(Value::from(false)),
            Some(1) => Some(Value::from(true)),
            _ => None,
        },
        (ColumnType::Json | ColumnType::JsonBinary, json) => {
            Some(Value::Json(Some(Box::new(json.clone()))))
        }
        (
            ColumnType::Char(_) | ColumnType::String(_) | ColumnType::Text,
            JsonValue::Number(_) | JsonValue::Bool(_),
        ) => None,
        (
            ColumnType::Date
            | ColumnType::DateTime
            | ColumnType::Timestamp
            | ColumnType::TimestampWithTimeZone
            | ColumnType::Time
            | ColumnType::Uuid,
            _,
        ) => None,
        (_, other) => Some(plain_value(other)?),
    };
    converted.ok_or_else(mismatch)
}

/// Convert a JSON scalar to a SQL value without a column type (HAVING clauses).
pub(crate) fn plain_value(value: &JsonValue) -> Result<Value> {
    match value {
        JsonValue::Bool(b) => Ok(Value::from(*b)),
        JsonValue::Number(n) => Ok(n
            .as_i64()
            .map(Value::from)
            .or_else(|| n.as_u64().map(Value::from))
            .unwrap_or_else(|| Value::from(n.as_f64().unwrap_or(f64::NAN)))),
        JsonValue::String(s) => Ok(Value::from(s.clone())),
        other => Err(QueryError::new(
            ErrorCode::InvalidFilterValue,
            format!("Expected a scalar value, got {}", json_type_name(other)),
        )),
    }
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .or_else(|| parse_datetime(s).map(|dt| dt.date()))
}

fn parse_datetime(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    DATETIME_FORMATS
        .iter()
        .find_map(|fmt| NaiveDateTime::parse_from_str(s, fmt).ok())
        .or_else(|| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.naive_utc())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .map(|d| d.and_time(NaiveTime::MIN))
        })
}

fn parse_datetime_tz(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .or_else(|| parse_datetime(s).map(|naive| naive.and_utc().fixed_offset()))
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    ["%H:%M:%S%.f", "%H:%M"]
        .iter()
        .find_map(|fmt| NaiveTime::parse_from_str(s, fmt).ok())
}

/// Parse the leading date of an ISO-like string (`2026-01-15`, `2026-01-15 10:00:00`,
/// `2026-01`, `2026`, `2026-Q1`), as returned by date-truncated group values.
pub(crate) fn parse_group_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if let Some(dt) = parse_datetime(s) {
        return Some(dt.date());
    }
    if let Ok(dt) = DateTime::<Utc>::from_str(s) {
        return Some(dt.date_naive());
    }
    if let Some((year, quarter)) = s.split_once("-Q") {
        let (year, quarter) = (year.parse::<i32>().ok()?, quarter.parse::<u32>().ok()?);
        return NaiveDate::from_ymd_opt(year, (quarter.checked_sub(1)?) * 3 + 1, 1);
    }
    NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(&format!("{s}-01-01"), "%Y-%m-%d").ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn col(column_type: ColumnType) -> ColumnInfo {
        ColumnInfo {
            name: "f",
            column_type,
            nullable: false,
            enum_type_name: None,
        }
    }

    #[test]
    fn coerces_temporal_strings() {
        let dt = NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        for raw in [
            "2026-01-15",
            "2026-01-15T00:00:00",
            "2026-01-15 00:00:00",
            "2026-01-15T00:00:00Z",
        ] {
            assert_eq!(
                column_value(&col(ColumnType::DateTime), "f", &json!(raw)).unwrap(),
                Value::from(dt)
            );
        }
        assert_eq!(
            column_value(&col(ColumnType::Date), "f", &json!("2026-01-15T10:30:00")).unwrap(),
            Value::from(dt.date())
        );
    }

    #[test]
    fn rejects_unparseable_values() {
        for (ty, raw) in [
            (ColumnType::DateTime, json!("not a date")),
            (ColumnType::Integer, json!("abc")),
            (ColumnType::Text, json!(5)),
            (ColumnType::Uuid, json!("nope")),
        ] {
            let err = column_value(&col(ty), "f", &raw).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidFilterValue);
        }
    }

    #[test]
    fn parses_group_dates() {
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(parse_group_date("2026-02-01"), d(2026, 2, 1));
        assert_eq!(parse_group_date("2026-02-01 00:00:00"), d(2026, 2, 1));
        assert_eq!(parse_group_date("2026-02"), d(2026, 2, 1));
        assert_eq!(parse_group_date("2026"), d(2026, 1, 1));
        assert_eq!(parse_group_date("2026-Q3"), d(2026, 7, 1));
        assert_eq!(parse_group_date("garbage"), None);
    }
}
