//! Builds the denormalized `search_text` string for a single document.
//!
//! Given an entity config with `search_text:` defined, this module:
//!   1. Takes the already-fetched own-table row (HashMap of column values)
//!   2. For each relation source, queries the target table via SQL
//!   3. Concatenates all text parts with the configured separator
//!
//! The full `Config` is required so the builder can resolve the actual
//! Postgres table name of any related entity (target name ≠ table name).

use crate::config::{Config, EntityConfig, RelationConfig, RelationKind, SearchTextConfig};
use anyhow::Result;
use sqlx::{PgPool, Row};
use std::collections::HashMap;

/// Build the `search_text` value for one document row.
///
/// `own_row` — already-fetched row from the entity's table (column → JSON value)
/// `pool`    — Postgres pool for relation lookups
/// `entity`  — this entity's config
/// `cfg`     — the `search_text:` sub-config
/// `config`  — full app config, used to resolve related entity table names
pub async fn build(
    own_row: &HashMap<String, serde_json::Value>,
    pool: &PgPool,
    entity: &EntityConfig,
    cfg: &SearchTextConfig,
    config: &Config,
) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();

    for source in &cfg.sources {
        match (&source.column, &source.relation) {
            // ── Own column ────────────────────────────────────────────────
            (Some(col_name), _) => {
                if let Some(val) = own_row.get(col_name) {
                    let text = value_to_text(val);
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
            }

            // ── Relation source ───────────────────────────────────────────
            (None, Some(rel_name)) => {
                if source.columns.is_empty() {
                    tracing::warn!(
                        "search_text relation source `{rel_name}` on `{}` has no columns listed",
                        entity.name
                    );
                    continue;
                }

                let rel = match entity.relations.iter().find(|r| &r.field == rel_name) {
                    Some(r) => r,
                    None => {
                        tracing::warn!(
                            "search_text source references unknown relation `{rel_name}` \
                             on entity `{}`",
                            entity.name
                        );
                        continue;
                    }
                };

                // Resolve actual PG table name:
                // 1. explicit target_table on the relation (user override)
                // 2. look up entity by name in config → use its .table field
                // 3. fall back to lowercasing the target name
                let target_table: String = rel.target_table.clone().unwrap_or_else(|| {
                    config
                        .entity(&rel.target)
                        .map(|e| e.table.clone())
                        .unwrap_or_else(|| rel.target.to_lowercase())
                });

                let texts = fetch_relation_texts(
                    own_row,
                    pool,
                    entity,
                    rel,
                    &source.columns,
                    &target_table,
                )
                .await?;
                parts.extend(texts);
            }

            // ── Neither set ───────────────────────────────────────────────
            (None, None) => {
                tracing::warn!(
                    "search_text source on `{}` has neither `column` nor `relation` set",
                    entity.name
                );
            }
        }
    }

    Ok(parts.join(&cfg.separator))
}

// ── SQL fetchers ──────────────────────────────────────────────────────────

async fn fetch_relation_texts(
    own_row: &HashMap<String, serde_json::Value>,
    pool: &PgPool,
    entity: &EntityConfig,
    rel: &RelationConfig,
    rel_cols: &[String],
    target_table: &str,
) -> Result<Vec<String>> {
    let col_list = rel_cols.join(", ");

    let rows = match rel.kind {
        // Many-to-one: look up the one related row by FK
        RelationKind::BelongsTo => {
            let fk = match own_row.get(&rel.local_col) {
                Some(v) if *v != serde_json::Value::Null => value_to_text(v),
                _ => return Ok(vec![]),
            };
            let sql = format!(
                "SELECT row_to_json(t)::TEXT AS _row \
                 FROM (SELECT {col_list} FROM {target_table} \
                       WHERE {} = '{}' LIMIT 1) t",
                rel.foreign_col,
                fk.replace('\'', "''")
            );
            query_rows(pool, &sql).await?
        }

        // One-to-many: collect all related rows
        RelationKind::HasMany => {
            let pk = match own_row.get(&entity.id_column) {
                Some(v) if *v != serde_json::Value::Null => value_to_text(v),
                _ => return Ok(vec![]),
            };
            let mut filter = format!("{} = '{}'", rel.foreign_col, pk.replace('\'', "''"));
            if let Some(ref extra) = rel.filter {
                filter.push_str(&format!(" AND ({extra})"));
            }
            let order = rel.order_by.as_deref().unwrap_or("1");
            let sql = format!(
                "SELECT row_to_json(t)::TEXT AS _row \
                 FROM (SELECT {col_list} FROM {target_table} \
                       WHERE {filter} ORDER BY {order} LIMIT {}) t",
                rel.limit
            );
            query_rows(pool, &sql).await?
        }

        // Many-to-many: join through the junction table
        RelationKind::ManyToMany => {
            let join_table = match &rel.join_table {
                Some(t) => t.as_str(),
                None => {
                    tracing::warn!("many_to_many relation `{}` missing join_table", rel.field);
                    return Ok(vec![]);
                }
            };
            let pk = match own_row.get(&entity.id_column) {
                Some(v) if *v != serde_json::Value::Null => value_to_text(v),
                _ => return Ok(vec![]),
            };
            // Qualify target columns to avoid ambiguity
            let qualified = rel_cols
                .iter()
                .map(|c| format!("t.{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT row_to_json(r)::TEXT AS _row FROM (\
                    SELECT {qualified} \
                    FROM {join_table} j \
                    JOIN {target_table} t ON t.{} = j.{} \
                    WHERE j.{} = '{}' \
                    LIMIT {}\
                 ) r",
                rel.target_id_col,
                rel.foreign_col,
                rel.local_col,
                pk.replace('\'', "''"),
                rel.limit
            );
            query_rows(pool, &sql).await?
        }
    };

    let texts: Vec<String> = rows
        .iter()
        .flat_map(|row| {
            rel_cols
                .iter()
                .filter_map(|col| row.get(col).map(value_to_text).filter(|s| !s.is_empty()))
        })
        .collect();

    Ok(texts)
}

async fn query_rows(pool: &PgPool, sql: &str) -> Result<Vec<HashMap<String, serde_json::Value>>> {
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            let txt: String = r.try_get("_row")?;
            let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&txt)?;
            Ok(obj.into_iter().collect())
        })
        .collect()
}

/// Convert any JSON value to a flat text string suitable for search indexing.
pub fn value_to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(value_to_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Object(obj) => obj
            .values()
            .map(value_to_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    }
}
