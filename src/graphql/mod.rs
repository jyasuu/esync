pub mod search;

use crate::{
    config::{Config, EntityConfig, PgType, RelationConfig, RelationKind},
    elastic::EsClient,
};
use anyhow::Result;
use async_graphql::dynamic::*;
use async_graphql::{Name, Value as GqlValue};
use sqlx::PgPool;
use std::sync::Arc;

// ── Public entry point ────────────────────────────────────────────────────

/// Build a fully dynamic GraphQL schema from the entity configs.
///
/// For each entity this generates:
///   - `list_<entity>(limit, offset, search, filter)` — paginated list
///   - `get_<entity>(id!)`                            — single record by PK
///
/// Scalar fields come from `entity.columns`.
/// Relationship fields come from `entity.relations` and resolve lazily via SQL.
pub fn build_schema(cfg: &Config, pool: Arc<PgPool>, es: Arc<EsClient>) -> Result<Schema> {
    let cfg = Arc::new(cfg.clone());
    let mut query = Object::new("Query");
    let mut builder = Schema::build("Query", None, None);

    for entity in &cfg.entities {
        let entity = entity.clone();

        // ── Object type with scalar fields + relation fields ──────────────
        let obj = build_object_type(&entity, Arc::clone(&cfg), Arc::clone(&pool));
        builder = builder.register(obj);

        // ── list_<entity> query ───────────────────────────────────────────
        let list_field = build_list_field(&entity, Arc::clone(&pool));
        query = query.field(list_field);

        // ── get_<entity> query ────────────────────────────────────────────
        let get_field = build_get_field(&entity, Arc::clone(&pool));
        query = query.field(get_field);
    }

    // Register search_* fields for entities with search.enabled
    let (builder, query) =
        search::register_search(&cfg, Arc::clone(&pool), Arc::clone(&es), builder, query);

    Ok(builder.register(query).finish()?)
}

// ── Object type builder ───────────────────────────────────────────────────

fn build_object_type(entity: &EntityConfig, cfg: Arc<Config>, pool: Arc<PgPool>) -> Object {
    let mut obj = Object::new(&entity.name);

    // ── Scalar fields ─────────────────────────────────────────────────────
    for col in &entity.columns {
        if !col.graphql {
            continue;
        }
        let col_name = col.name.clone();
        let gql_type = pg_to_gql_type_pub(&col.pg_type);
        obj = obj.field(Field::new(col_name.clone(), gql_type, move |ctx| {
            let name = col_name.clone();
            FieldFuture::new(async move {
                let val = match ctx.parent_value.as_value() {
                    Some(GqlValue::Object(map)) => {
                        map.get(name.as_str()).cloned().unwrap_or(GqlValue::Null)
                    }
                    _ => GqlValue::Null,
                };
                Ok(Some(val))
            })
        }));
    }

    // ── Relation fields ───────────────────────────────────────────────────
    for rel in &entity.relations {
        let rel = rel.clone();
        let cfg_r = Arc::clone(&cfg);
        let pool_r = Arc::clone(&pool);

        let return_type = match rel.kind {
            RelationKind::BelongsTo => TypeRef::named(&rel.target), // nullable single
            RelationKind::HasMany | RelationKind::ManyToMany => {
                TypeRef::named_nn_list_nn(&rel.target)
            } // non-null list
        };

        // For ManyToMany, local_col is the join-table column name, not a parent column.
        // Capture the entity's own PK before the closure so we can use it inside.
        let entity_id_col = entity.id_column.clone();
        let field_name = rel.field.clone();
        obj = obj.field(Field::new(field_name, return_type, move |ctx| {
            let rel = rel.clone();
            let entity_id_col = entity_id_col.clone();
            let cfg = Arc::clone(&cfg_r);
            let pool = Arc::clone(&pool_r);

            FieldFuture::new(async move {
                // For ManyToMany the parent row has the entity's PK (entity_id_col),
                // not rel.local_col (which names the join-table column).
                let parent_field = match rel.kind {
                    RelationKind::ManyToMany => entity_id_col.as_str(),
                    _ => rel.local_col.as_str(),
                };
                let local_val = match ctx.parent_value.as_value() {
                    Some(GqlValue::Object(map)) => {
                        map.get(parent_field).cloned().unwrap_or(GqlValue::Null)
                    }
                    _ => GqlValue::Null,
                };

                // Null local key → null / empty list
                if local_val == GqlValue::Null {
                    return Ok(match rel.kind {
                        RelationKind::BelongsTo => None,
                        _ => Some(GqlValue::List(vec![])),
                    });
                }

                let local_str = gql_value_to_sql_literal(&local_val);

                let target = cfg.entity(&rel.target).ok_or_else(|| {
                    async_graphql::Error::new(format!("Unknown relation target: {}", rel.target))
                })?;

                let cols: Vec<&str> = target
                    .columns
                    .iter()
                    .filter(|c| c.graphql)
                    .map(|c| c.name.as_str())
                    .collect();

                let rows = match rel.kind {
                    RelationKind::BelongsTo => {
                        fetch_belongs_to(&pool, target, &cols, &rel, &local_str).await?
                    }
                    RelationKind::HasMany => {
                        fetch_has_many(&pool, target, &cols, &rel, &local_str).await?
                    }
                    RelationKind::ManyToMany => {
                        fetch_many_to_many(&pool, target, &cols, &rel, &local_str).await?
                    }
                };

                let items: Vec<GqlValue> = rows.into_iter().map(row_to_gql).collect();

                Ok(match rel.kind {
                    RelationKind::BelongsTo => items.into_iter().next().map(Some).unwrap_or(None),
                    _ => Some(GqlValue::List(items)),
                })
            })
        }));
    }

    obj
}

// ── SQL fetchers for each relation kind ───────────────────────────────────

async fn fetch_belongs_to(
    pool: &PgPool,
    target: &EntityConfig,
    cols: &[&str],
    rel: &RelationConfig,
    local_val: &str,
) -> Result<Vec<std::collections::HashMap<String, serde_json::Value>>> {
    let filter = format!(
        "{} = {}{}",
        rel.foreign_col,
        local_val,
        rel.filter
            .as_ref()
            .map(|f| format!(" AND ({f})"))
            .unwrap_or_default()
    );
    crate::db::fetch_rows(pool, &target.table, cols, Some(&filter), 1, 0).await
}

async fn fetch_has_many(
    pool: &PgPool,
    target: &EntityConfig,
    cols: &[&str],
    rel: &RelationConfig,
    local_val: &str,
) -> Result<Vec<std::collections::HashMap<String, serde_json::Value>>> {
    let mut filter = format!("{} = {}", rel.foreign_col, local_val);
    if let Some(ref extra) = rel.filter {
        filter.push_str(&format!(" AND ({extra})"));
    }
    if let Some(ref target_filter) = target.filter {
        filter.push_str(&format!(" AND ({target_filter})"));
    }
    let order = rel.order_by.as_deref().unwrap_or("1");
    // Simpler: just use fetch_rows with a composite filter + override limit
    fetch_with_order(pool, target, cols, &filter, order, rel.limit).await
}

async fn fetch_many_to_many(
    pool: &PgPool,
    target: &EntityConfig,
    cols: &[&str],
    rel: &RelationConfig,
    local_val: &str,
) -> Result<Vec<std::collections::HashMap<String, serde_json::Value>>> {
    let join_table = rel.join_table.as_deref().ok_or_else(|| {
        anyhow::anyhow!("many_to_many relation '{}' missing join_table", rel.field)
    })?;

    let mut filter_parts = vec![format!(
        "{}.{} IN (SELECT {} FROM {join_table} WHERE {} = {})",
        target.table, rel.target_id_col, rel.foreign_col, rel.local_col, local_val
    )];
    if let Some(ref f) = rel.filter {
        filter_parts.push(format!("({f})"));
    }
    if let Some(ref f) = target.filter {
        filter_parts.push(format!("({f})"));
    }
    let filter = filter_parts.join(" AND ");
    let order = rel.order_by.as_deref().unwrap_or("1");
    fetch_with_order(pool, target, cols, &filter, order, rel.limit).await
}

/// Like `db::fetch_rows` but with explicit ORDER BY and LIMIT instead of fixed "ORDER BY 1".
async fn fetch_with_order(
    pool: &PgPool,
    target: &EntityConfig,
    cols: &[&str],
    filter: &str,
    order: &str,
    limit: i64,
) -> Result<Vec<std::collections::HashMap<String, serde_json::Value>>> {
    use sqlx::Row;
    let col_list = cols
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {} WHERE {filter} ORDER BY {order} LIMIT {limit}) t",
        target.table
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

// ── list / get query fields ───────────────────────────────────────────────

fn build_list_field(entity: &EntityConfig, pool: Arc<PgPool>) -> Field {
    let entity_c = entity.clone();
    let list_name = format!("list_{}", snake_pub(&entity.name));

    Field::new(
        list_name,
        TypeRef::named_nn_list_nn(&entity.name),
        move |ctx| {
            let pool = Arc::clone(&pool);
            let entity = entity_c.clone();
            FieldFuture::new(async move {
                let limit: i64 = ctx
                    .args
                    .get("limit")
                    .and_then(|v| v.i64().ok())
                    .unwrap_or(20);
                let offset: i64 = ctx
                    .args
                    .get("offset")
                    .and_then(|v| v.i64().ok())
                    .unwrap_or(0);
                let search = ctx
                    .args
                    .get("search")
                    .and_then(|v| v.string().ok().map(str::to_owned));
                // Extra ad-hoc filter (SQL WHERE fragment)
                let extra_filter = ctx
                    .args
                    .get("filter")
                    .and_then(|v| v.string().ok().map(str::to_owned));

                let cols: Vec<&str> = entity
                    .columns
                    .iter()
                    .filter(|c| c.graphql)
                    .map(|c| c.name.as_str())
                    .collect();

                let filter = build_filter(&entity, search.as_deref(), extra_filter.as_deref());
                let rows = crate::db::fetch_rows(
                    &pool,
                    &entity.table,
                    &cols,
                    filter.as_deref(),
                    limit,
                    offset,
                )
                .await?;

                Ok(Some(GqlValue::List(
                    rows.into_iter().map(row_to_gql).collect(),
                )))
            })
        },
    )
    .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
    .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT)))
    .argument(InputValue::new("search", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("filter", TypeRef::named(TypeRef::STRING)))
}

fn build_get_field(entity: &EntityConfig, pool: Arc<PgPool>) -> Field {
    let entity_g = entity.clone();
    let get_name = format!("get_{}", snake_pub(&entity.name));

    Field::new(get_name, TypeRef::named(&entity.name), move |ctx| {
        let pool = Arc::clone(&pool);
        let entity = entity_g.clone();
        FieldFuture::new(async move {
            let id: String = ctx
                .args
                .get("id")
                .and_then(|v| v.string().ok().map(str::to_owned))
                .ok_or_else(|| async_graphql::Error::new("id is required"))?;

            let cols: Vec<&str> = entity
                .columns
                .iter()
                .filter(|c| c.graphql)
                .map(|c| c.name.as_str())
                .collect();

            let filter = format!("{} = '{}'", entity.id_column, id.replace('\'', "''"));
            let mut rows =
                crate::db::fetch_rows(&pool, &entity.table, &cols, Some(&filter), 1, 0).await?;

            Ok(rows.pop().map(row_to_gql))
        })
    })
    .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::STRING)))
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn row_to_gql(row: std::collections::HashMap<String, serde_json::Value>) -> GqlValue {
    GqlValue::Object(
        row.into_iter()
            .map(|(k, v)| (Name::new(k), json_to_gql(v)))
            .collect::<indexmap::IndexMap<Name, GqlValue>>(),
    )
}

/// Convert a GqlValue scalar to a SQL literal string suitable for WHERE clauses.
fn gql_value_to_sql_literal(v: &GqlValue) -> String {
    match v {
        GqlValue::String(s) => format!("'{}'", s.replace('\'', "''")),
        GqlValue::Number(n) => n.to_string(),
        GqlValue::Boolean(b) => b.to_string(),
        _ => "NULL".to_string(),
    }
}

pub fn snake_pub(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

pub fn pg_to_gql_type_pub(pg: &PgType) -> TypeRef {
    match pg {
        PgType::Bool => TypeRef::named(TypeRef::BOOLEAN),
        PgType::Int2 | PgType::Int4 => TypeRef::named(TypeRef::INT),
        PgType::Int8 | PgType::Numeric | PgType::Float4 | PgType::Float8 => {
            TypeRef::named(TypeRef::FLOAT)
        }
        _ => TypeRef::named(TypeRef::STRING),
    }
}

fn build_filter(
    entity: &EntityConfig,
    search: Option<&str>,
    extra_filter: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(base) = &entity.filter {
        parts.push(format!("({base})"));
    }
    if let Some(q) = search {
        let text_cols: Vec<String> = entity
            .columns
            .iter()
            .filter(|c| matches!(c.pg_type, PgType::Text | PgType::Varchar))
            .map(|c| format!("{} ILIKE '%{}%'", c.name, q.replace('\'', "''")))
            .collect();
        if !text_cols.is_empty() {
            parts.push(format!("({})", text_cols.join(" OR ")));
        }
    }
    if let Some(f) = extra_filter {
        parts.push(format!("({f})"));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

fn json_to_gql(v: serde_json::Value) -> GqlValue {
    match v {
        serde_json::Value::Null => GqlValue::Null,
        serde_json::Value::Bool(b) => GqlValue::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                GqlValue::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                GqlValue::Number(async_graphql::Number::from_f64(f).unwrap_or_else(|| 0i32.into()))
            } else {
                GqlValue::Null
            }
        }
        serde_json::Value::String(s) => GqlValue::String(s),
        serde_json::Value::Array(arr) => GqlValue::List(arr.into_iter().map(json_to_gql).collect()),
        serde_json::Value::Object(obj) => GqlValue::Object(
            obj.into_iter()
                .map(|(k, v)| (Name::new(k), json_to_gql(v)))
                .collect::<indexmap::IndexMap<Name, GqlValue>>(),
        ),
    }
}
