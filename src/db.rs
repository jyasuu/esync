use anyhow::Result;
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::collections::HashMap;

pub async fn connect(url: &str, pool_size: u32) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(pool_size)
        .connect(url)
        .await?;
    tracing::info!("Connected to PostgreSQL");
    Ok(pool)
}

/// Fetch a page of rows from `table` as column-name → JSON value maps.
pub async fn fetch_rows(
    pool:    &PgPool,
    table:   &str,
    columns: &[&str],
    filter:  Option<&str>,
    limit:   i64,
    offset:  i64,
) -> Result<Vec<HashMap<String, serde_json::Value>>> {
    let col_list     = columns.join(", ");
    let where_clause = filter
        .map(|f| format!("WHERE {f}"))
        .unwrap_or_default();

    let sql = format!(
        "SELECT {col_list} FROM {table} {where_clause} ORDER BY 1 LIMIT {limit} OFFSET {offset}"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    rows.iter()
        .map(|row| row_to_map(row, columns))
        .collect()
}

/// Count rows in `table` matching the optional filter.
pub async fn count_rows(
    pool:   &PgPool,
    table:  &str,
    filter: Option<&str>,
) -> Result<i64> {
    let where_clause = filter
        .map(|f| format!("WHERE {f}"))
        .unwrap_or_default();
    let sql = format!("SELECT COUNT(*) FROM {table} {where_clause}");
    let (count,): (i64,) = sqlx::query_as(&sql).fetch_one(pool).await?;
    Ok(count)
}

fn row_to_map(
    row:     &sqlx::postgres::PgRow,
    columns: &[&str],
) -> Result<HashMap<String, serde_json::Value>> {
    let mut map = HashMap::new();
    for &col in columns {
        map.insert(col.to_string(), try_decode(row, col));
    }
    Ok(map)
}

/// Attempt each concrete Postgres type; the first successful decode wins.
/// Falls back to String, then Null.
fn try_decode(row: &sqlx::postgres::PgRow, col: &str) -> serde_json::Value {
    // bool — must come before integers to avoid mis-typing
    if let Ok(v) = row.try_get::<Option<bool>, _>(col) {
        return v.map(serde_json::Value::Bool).unwrap_or(serde_json::Value::Null);
    }
    // integers
    if let Ok(v) = row.try_get::<Option<i32>, _>(col) {
        return v
            .map(|n| serde_json::Value::Number(n.into()))
            .unwrap_or(serde_json::Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(col) {
        return v
            .map(|n| serde_json::Value::Number(n.into()))
            .unwrap_or(serde_json::Value::Null);
    }
    // floats
    if let Ok(v) = row.try_get::<Option<f64>, _>(col) {
        return v
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
    }
    // JSONB / JSON
    if let Ok(v) = row.try_get::<Option<serde_json::Value>, _>(col) {
        return v.unwrap_or(serde_json::Value::Null);
    }
    // Everything else as text
    if let Ok(v) = row.try_get::<Option<String>, _>(col) {
        return v.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null);
    }
    serde_json::Value::Null
}
