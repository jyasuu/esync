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
    pool: &PgPool,
    table: &str,
    columns: &[&str],
    filter: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<HashMap<String, serde_json::Value>>> {
    let col_list = columns.join(", ");
    let where_clause = filter.map(|f| format!("WHERE {f}")).unwrap_or_default();

    // Cast every column to TEXT so we get a stable, unambiguous string
    // representation regardless of the underlying PG type. ES receives the
    // strings and coerces them via its own mappings (date, scaled_float, etc.).
    let cast_list = columns
        .iter()
        .map(|c| format!("{c}::TEXT AS {c}"))
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        "SELECT {cast_list} FROM {table} {where_clause} ORDER BY 1 LIMIT {limit} OFFSET {offset}"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    rows.iter().map(|row| row_to_map(row, columns)).collect()
}

/// Count rows in `table` matching the optional filter.
pub async fn count_rows(pool: &PgPool, table: &str, filter: Option<&str>) -> Result<i64> {
    let where_clause = filter.map(|f| format!("WHERE {f}")).unwrap_or_default();
    let sql = format!("SELECT COUNT(*) FROM {table} {where_clause}");
    let (count,): (i64,) = sqlx::query_as(&sql).fetch_one(pool).await?;
    Ok(count)
}

fn row_to_map(
    row: &sqlx::postgres::PgRow,
    columns: &[&str],
) -> Result<HashMap<String, serde_json::Value>> {
    let mut map = HashMap::new();
    for &col in columns {
        // Every column was cast to TEXT in the query, so we only need String.
        let val = row
            .try_get::<Option<String>, _>(col)
            .unwrap_or(None)
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        map.insert(col.to_string(), val);
    }
    Ok(map)
}
