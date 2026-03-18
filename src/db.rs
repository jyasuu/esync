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
///
/// Every column is cast to a canonical representation:
/// - TIMESTAMPTZ / TIMESTAMP → ISO-8601 string via `AT TIME ZONE 'UTC'`
/// - NUMERIC                 → plain text (ES scaled_float accepts "9.99")
/// - Everything else         → TEXT cast
///
/// This avoids sqlx's type-guessing (which mis-decodes UUIDs as bool) and
/// keeps the values in formats ES accepts through its mappings.
pub async fn fetch_rows(
    pool:    &PgPool,
    table:   &str,
    columns: &[&str],
    filter:  Option<&str>,
    limit:   i64,
    offset:  i64,
) -> Result<Vec<HashMap<String, serde_json::Value>>> {
    let where_clause = filter
        .map(|f| format!("WHERE {f}"))
        .unwrap_or_default();

    // Use to_json() so Postgres handles type coercion natively:
    //   - timestamps become ISO-8601 strings with timezone
    //   - numbers stay as JSON numbers (not strings)
    //   - booleans stay as JSON booleans
    //   - nulls become JSON null
    //   - UUIDs become strings
    // row_to_json wraps the whole row; we unwrap it per-column below.
    let col_list = columns.join(", ");
    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {table} {where_clause} ORDER BY 1 LIMIT {limit} OFFSET {offset}) t"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    rows.iter()
        .map(|row| {
            let json_text: String = row.try_get("_row")?;
            let obj: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&json_text)?;
            Ok(obj.into_iter().collect())
        })
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

/// Fetch rows by a list of id values — used for batch enrichment after ES search.
/// Returns a HashMap keyed by the id value for O(1) lookup.
pub async fn fetch_by_ids(
    pool:     &PgPool,
    table:    &str,
    id_col:   &str,
    columns:  &[&str],
    ids:      &[String],
    filter:   Option<&str>,
) -> Result<HashMap<String, HashMap<String, serde_json::Value>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let col_list = columns.join(", ");
    let id_list  = ids.iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");

    let extra = filter
        .map(|f| format!(" AND ({f})"))
        .unwrap_or_default();

    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {table} \
               WHERE {id_col} IN ({id_list}){extra}) t"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    let mut map: HashMap<String, HashMap<String, serde_json::Value>> = HashMap::new();
    for row in &rows {
        let json_text: String = row.try_get("_row")?;
        let obj: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&json_text)?;
        let row_map: HashMap<String, serde_json::Value> = obj.into_iter().collect();
        if let Some(id_val) = row_map.get(id_col) {
            let id_str = match id_val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            };
            map.insert(id_str, row_map);
        }
    }
    Ok(map)
}
