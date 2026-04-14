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

/// Fetch a page of rows from `source` (a table name or sub-select expression)
/// as column-name → JSON value maps.
///
/// Every column is cast to a canonical representation via `row_to_json` so
/// Postgres handles type coercion natively (timestamps → ISO-8601, UUIDs →
/// strings, booleans → booleans, nulls → null).
pub async fn fetch_rows(
    pool: &PgPool,
    source: &str,
    columns: &[&str],
    filter: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<HashMap<String, serde_json::Value>>> {
    let where_clause = filter.map(|f| format!("WHERE {f}")).unwrap_or_default();

    let col_list = if columns.is_empty() {
        "*".to_string()
    } else {
        columns.join(", ")
    };
    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {source} {where_clause} ORDER BY 1 LIMIT {limit} OFFSET {offset}) t"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    rows.iter()
        .map(|row| {
            let json_text: String = row.try_get("_row")?;
            let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
            Ok(obj.into_iter().collect())
        })
        .collect()
}

/// Count rows in `source` (table name or sub-select) matching the optional filter.
pub async fn count_rows(pool: &PgPool, source: &str, filter: Option<&str>) -> Result<i64> {
    let where_clause = filter.map(|f| format!("WHERE {f}")).unwrap_or_default();
    let sql = format!("SELECT COUNT(*) FROM {source} {where_clause}");
    let (count,): (i64,) = sqlx::query_as(&sql).fetch_one(pool).await?;
    Ok(count)
}

// ── Write helpers (used by mutations) ────────────────────────────────────

/// INSERT a row into `table` and return the inserted row as a JSON map.
/// `fields` is a list of (column_name, sql_literal) pairs.
pub async fn insert_row(
    pool: &PgPool,
    table: &str,
    fields: &[(String, String)],
    returning_cols: &[&str],
) -> Result<HashMap<String, serde_json::Value>> {
    if fields.is_empty() {
        anyhow::bail!("insert_row: no fields provided");
    }
    let col_names: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    let col_values: Vec<&str> = fields.iter().map(|(_, v)| v.as_str()).collect();
    let ret_cols = if returning_cols.is_empty() {
        "*".to_string()
    } else {
        returning_cols.join(", ")
    };
    // Use a CTE so we can wrap the RETURNING clause in row_to_json.
    let sql = format!(
        "WITH _ins AS (INSERT INTO {table} ({cols}) VALUES ({vals}) RETURNING {ret_cols}) \
         SELECT row_to_json(_ins)::TEXT AS _row FROM _ins",
        cols = col_names.join(", "),
        vals = col_values.join(", "),
    );
    let row = sqlx::query(&sql).fetch_one(pool).await?;
    let json_text: String = row.try_get("_row")?;
    let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
    Ok(obj.into_iter().collect())
}

/// UPDATE a row in `table` and return the updated row.
/// `fields` is a list of (column_name, sql_literal) pairs.
pub async fn update_row(
    pool: &PgPool,
    table: &str,
    id_col: &str,
    id_val: &str,
    fields: &[(String, String)],
    returning_cols: &[&str],
) -> Result<Option<HashMap<String, serde_json::Value>>> {
    if fields.is_empty() {
        anyhow::bail!("update_row: no fields provided");
    }
    let set_clause = fields
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ret_cols = if returning_cols.is_empty() {
        "*".to_string()
    } else {
        returning_cols.join(", ")
    };
    let sql = format!(
        "WITH _upd AS (UPDATE {table} SET {set_clause} WHERE {id_col} = {id_val} RETURNING {ret_cols}) \
         SELECT row_to_json(_upd)::TEXT AS _row FROM _upd"
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    if let Some(row) = rows.into_iter().next() {
        let json_text: String = row.try_get("_row")?;
        let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
        Ok(Some(obj.into_iter().collect()))
    } else {
        Ok(None)
    }
}

/// DELETE a row from `table` by primary key. Returns true if a row was deleted.
pub async fn delete_row(pool: &PgPool, table: &str, id_col: &str, id_val: &str) -> Result<bool> {
    let sql = format!("DELETE FROM {table} WHERE {id_col} = {id_val}");
    let result = sqlx::query(&sql).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Fetch rows by a list of id values — used for batch enrichment after ES search.
/// Returns a HashMap keyed by the id value for O(1) lookup.
pub async fn fetch_by_ids(
    pool: &PgPool,
    table: &str,
    id_col: &str,
    columns: &[&str],
    ids: &[String],
    filter: Option<&str>,
) -> Result<HashMap<String, HashMap<String, serde_json::Value>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let col_list = columns.join(", ");
    let id_list = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");

    let extra = filter.map(|f| format!(" AND ({f})")).unwrap_or_default();

    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {table} \
               WHERE {id_col} IN ({id_list}){extra}) t"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    let mut map: HashMap<String, HashMap<String, serde_json::Value>> = HashMap::new();
    for row in &rows {
        let json_text: String = row.try_get("_row")?;
        let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
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

// ── RLS-aware helpers ─────────────────────────────────────────────────────
//
// These functions run a user-visible query inside an explicit transaction,
// emit `SET LOCAL rls.<key> = '<value>'` for each auth context parameter
// before the query, then commit.  PostgreSQL RLS policies can then reference
// `current_setting('rls.user_id', true)` etc.
//
// We use SET LOCAL (not SET) so the settings are scoped to this transaction
// and don't leak to the next connection from the pool.

/// Execute `SET LOCAL <key> = '<value>'` for each RLS parameter inside an
/// open sqlx transaction.
///
/// The value is a plain string for `request.jwt.token_type` and a compact
/// JSON string for `request.jwt.claims`.  Single quotes in the value are
/// doubled (`''`) per Postgres string literal rules.  JSON uses `\"` for
/// internal strings so no additional escaping is needed for JSON content.
pub async fn set_rls_params(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    params: &[(String, String)],
) -> Result<()> {
    for (key, val) in params {
        // Double single quotes to produce a valid Postgres string literal.
        let escaped = val.replace('\'', "''");
        let sql = format!("SET LOCAL {key} = '{escaped}'");
        sqlx::query(&sql).execute(&mut **tx).await?;
    }
    Ok(())
}

/// Fetch rows with RLS parameters set for this transaction.
///
/// When `rls_params` is empty (OAuth2 not configured) the fast-path is taken —
/// no transaction wrapper.  When OAuth2 IS configured, `rls_params` always
/// contains at least `request.jwt.token_type`, so the transaction always runs
/// and Postgres GUC values are visible to RLS policies.
pub async fn fetch_rows_rls(
    pool: &PgPool,
    source: &str,
    columns: &[&str],
    filter: Option<&str>,
    limit: i64,
    offset: i64,
    rls_params: &[(String, String)],
) -> Result<Vec<HashMap<String, serde_json::Value>>> {
    if rls_params.is_empty() {
        // Fast path: no auth context, bypass transaction overhead.
        return fetch_rows(pool, source, columns, filter, limit, offset).await;
    }

    let mut tx = pool.begin().await?;
    set_rls_params(&mut tx, rls_params).await?;

    let where_clause = filter.map(|f| format!("WHERE {f}")).unwrap_or_default();
    let col_list = if columns.is_empty() {
        "*".to_string()
    } else {
        columns.join(", ")
    };
    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {source} {where_clause} ORDER BY 1 LIMIT {limit} OFFSET {offset}) t"
    );

    use sqlx::Row;
    let rows = sqlx::query(&sql).fetch_all(&mut *tx).await?;
    tx.commit().await?;

    rows.iter()
        .map(|row| {
            let json_text: String = row.try_get("_row")?;
            let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
            Ok(obj.into_iter().collect())
        })
        .collect()
}

/// INSERT with RLS params.
pub async fn insert_row_rls(
    pool: &PgPool,
    table: &str,
    fields: &[(String, String)],
    returning_cols: &[&str],
    rls_params: &[(String, String)],
) -> Result<HashMap<String, serde_json::Value>> {
    if rls_params.is_empty() {
        return insert_row(pool, table, fields, returning_cols).await;
    }

    let mut tx = pool.begin().await?;
    set_rls_params(&mut tx, rls_params).await?;

    if fields.is_empty() {
        anyhow::bail!("insert_row_rls: no fields provided");
    }
    let col_names: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    let col_values: Vec<&str> = fields.iter().map(|(_, v)| v.as_str()).collect();
    let ret_cols = if returning_cols.is_empty() {
        "*".to_string()
    } else {
        returning_cols.join(", ")
    };
    let sql = format!(
        "WITH _ins AS (INSERT INTO {table} ({cols}) VALUES ({vals}) RETURNING {ret_cols}) \
         SELECT row_to_json(_ins)::TEXT AS _row FROM _ins",
        cols = col_names.join(", "),
        vals = col_values.join(", "),
    );

    use sqlx::Row;
    let row = sqlx::query(&sql).fetch_one(&mut *tx).await?;
    tx.commit().await?;

    let json_text: String = row.try_get("_row")?;
    let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
    Ok(obj.into_iter().collect())
}

/// UPDATE with RLS params.
pub async fn update_row_rls(
    pool: &PgPool,
    table: &str,
    id_col: &str,
    id_val: &str,
    fields: &[(String, String)],
    returning_cols: &[&str],
    rls_params: &[(String, String)],
) -> Result<Option<HashMap<String, serde_json::Value>>> {
    if rls_params.is_empty() {
        return update_row(pool, table, id_col, id_val, fields, returning_cols).await;
    }

    let mut tx = pool.begin().await?;
    set_rls_params(&mut tx, rls_params).await?;

    if fields.is_empty() {
        anyhow::bail!("update_row_rls: no fields provided");
    }
    let set_clause = fields
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ret_cols = if returning_cols.is_empty() {
        "*".to_string()
    } else {
        returning_cols.join(", ")
    };
    let sql = format!(
        "WITH _upd AS (UPDATE {table} SET {set_clause} WHERE {id_col} = {id_val} RETURNING {ret_cols}) \
         SELECT row_to_json(_upd)::TEXT AS _row FROM _upd"
    );

    use sqlx::Row;
    let rows = sqlx::query(&sql).fetch_all(&mut *tx).await?;
    tx.commit().await?;

    if let Some(row) = rows.into_iter().next() {
        let json_text: String = row.try_get("_row")?;
        let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json_text)?;
        Ok(Some(obj.into_iter().collect()))
    } else {
        Ok(None)
    }
}

/// DELETE with RLS params.
pub async fn delete_row_rls(
    pool: &PgPool,
    table: &str,
    id_col: &str,
    id_val: &str,
    rls_params: &[(String, String)],
) -> Result<bool> {
    if rls_params.is_empty() {
        return delete_row(pool, table, id_col, id_val).await;
    }

    let mut tx = pool.begin().await?;
    set_rls_params(&mut tx, rls_params).await?;

    let sql = format!("DELETE FROM {table} WHERE {id_col} = {id_val}");
    let result = sqlx::query(&sql).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
