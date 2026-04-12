//! Mutation field builders.
//!
//! For each writable entity (not readonly, not sql-backed) this module
//! generates three root mutation fields:
//!
//!   create_<entity>(input: Create<Entity>Input!): <Entity>!
//!   update_<entity>(id: String!, input: Update<Entity>Input!): <Entity>
//!   delete_<entity>(id: String!): Boolean!
//!
//! Input types are derived from `entity.columns`:
//!   - `Create<Entity>Input` — all graphql:true cols except id_column, non-null
//!   - `Update<Entity>Input` — same cols but all nullable (partial patch)
//!
//! After every successful write the mutation immediately rebuilds search_text
//! and upserts the ES document so search stays fresh.

use crate::{
    auth::AuthContext,
    config::{Config, EntityConfig, OAuth2Config, PgType},
    db,
    elastic::EsClient,
    graphql::{pg_to_gql_type_pub, row_to_gql, snake_pub},
    indexer,
};
use async_graphql::dynamic::*;
use async_graphql::Value as GqlValue;
use sqlx::PgPool;
use std::sync::Arc;

// ── Input type names ──────────────────────────────────────────────────────

fn create_input_name(entity: &EntityConfig) -> String {
    format!("Create{}Input", entity.name)
}
fn update_input_name(entity: &EntityConfig) -> String {
    format!("Update{}Input", entity.name)
}

// ── Public: build both InputObjects ──────────────────────────────────────

/// Returns `(Create<Entity>Input, Update<Entity>Input)`.
/// The caller registers both with the schema builder.
pub fn build_all_input_types(entity: &EntityConfig) -> (InputObject, InputObject) {
    let mutable_cols: Vec<_> = entity
        .columns
        .iter()
        .filter(|c| c.graphql && c.name != entity.id_column)
        .collect();

    let mut create_input = InputObject::new(create_input_name(entity));
    let mut update_input = InputObject::new(update_input_name(entity));

    for col in &mutable_cols {
        let gql_type = pg_to_gql_type_pub(&col.pg_type);
        // Create: required (non-null); Update: optional (nullable)
        let required_type = match &gql_type {
            TypeRef::Named(n) => TypeRef::named_nn(n.as_ref()),
            TypeRef::NonNull(inner) => TypeRef::NonNull(inner.clone()),
            other => other.clone(),
        };
        create_input = create_input.field(InputValue::new(col.name.clone(), required_type));
        update_input = update_input.field(InputValue::new(col.name.clone(), gql_type));
    }

    (create_input, update_input)
}

// ── Public: build the three mutation Fields ───────────────────────────────

/// Build and return `(create_<entity>, update_<entity>, delete_<entity>)` Fields.
/// Call `build_all_input_types` first and register both InputObjects before
/// adding these fields to the Mutation object.
pub fn build_mutation_fields(
    entity: &EntityConfig,
    pool: Arc<PgPool>,
    es: Arc<EsClient>,
    cfg: Arc<Config>,
    oauth2_cfg: Option<Arc<OAuth2Config>>,
) -> (Field, Field, Field) {
    let create_tn = create_input_name(entity);
    let update_tn = update_input_name(entity);

    // ── create_<entity> ───────────────────────────────────────────────────
    let entity_c = entity.clone();
    let pool_c = Arc::clone(&pool);
    let es_c = Arc::clone(&es);
    let cfg_c = Arc::clone(&cfg);

    let oauth2_c = oauth2_cfg.clone();
    let create_field = Field::new(
        format!("create_{}", snake_pub(&entity.name)),
        TypeRef::named_nn(&entity.name),
        move |ctx| {
            let pool = Arc::clone(&pool_c);
            let es = Arc::clone(&es_c);
            let cfg = Arc::clone(&cfg_c);
            let entity = entity_c.clone();
            let oauth2_cfg = oauth2_c.clone();
            FieldFuture::new(async move {
                let rls_params = ctx
                    .data::<AuthContext>()
                    .ok()
                    .zip(oauth2_cfg.as_deref())
                    .map(|(a, c)| a.rls_params(c))
                    .unwrap_or_default();
                let input = ctx
                    .args
                    .get("input")
                    .and_then(|v| v.object().ok())
                    .ok_or_else(|| async_graphql::Error::new("input is required"))?;

                let mut fields: Vec<(String, String)> = Vec::new();

                // Optional client-supplied id
                if let Some(id_acc) = input.get(entity.id_column.as_str()) {
                    let id_val = id_acc.deserialize::<GqlValue>().unwrap_or(GqlValue::Null);
                    if id_val != GqlValue::Null {
                        fields.push((
                            entity.id_column.clone(),
                            gql_to_sql_literal(&id_val, &PgType::Uuid),
                        ));
                    }
                }

                for col in entity
                    .columns
                    .iter()
                    .filter(|c| c.graphql && c.name != entity.id_column)
                {
                    match input.get(col.name.as_str()) {
                        Some(acc) => {
                            let val = acc.deserialize::<GqlValue>().unwrap_or(GqlValue::Null);
                            fields.push((col.name.clone(), gql_to_sql_literal(&val, &col.pg_type)));
                        }
                        None => {
                            return Err(async_graphql::Error::new(format!(
                                "Field '{}' is required for create",
                                col.name
                            )));
                        }
                    }
                }

                let returning: Vec<&str> = entity.columns.iter().map(|c| c.name.as_str()).collect();
                let row =
                    db::insert_row_rls(&pool, &entity.table, &fields, &returning, &rls_params)
                        .await?;

                let id = extract_id(&row, &entity.id_column);
                sync_to_es(&pool, &es, &entity, &id, row.clone(), &cfg).await;
                Ok(Some(row_to_gql(row)))
            })
        },
    )
    .argument(InputValue::new("input", TypeRef::named_nn(&create_tn)));

    // ── update_<entity> ───────────────────────────────────────────────────
    let entity_u = entity.clone();
    let pool_u = Arc::clone(&pool);
    let es_u = Arc::clone(&es);
    let cfg_u = Arc::clone(&cfg);
    let oauth2_u = oauth2_cfg.clone();

    let update_field = Field::new(
        format!("update_{}", snake_pub(&entity.name)),
        TypeRef::named(&entity.name),
        move |ctx| {
            let pool = Arc::clone(&pool_u);
            let es = Arc::clone(&es_u);
            let cfg = Arc::clone(&cfg_u);
            let entity = entity_u.clone();
            let oauth2_cfg = oauth2_u.clone();
            FieldFuture::new(async move {
                let rls_params = ctx
                    .data::<AuthContext>()
                    .ok()
                    .zip(oauth2_cfg.as_deref())
                    .map(|(a, c)| a.rls_params(c))
                    .unwrap_or_default();
                let id: String = ctx
                    .args
                    .get("id")
                    .and_then(|v| v.string().ok().map(str::to_owned))
                    .ok_or_else(|| async_graphql::Error::new("id is required"))?;

                let input = ctx
                    .args
                    .get("input")
                    .and_then(|v| v.object().ok())
                    .ok_or_else(|| async_graphql::Error::new("input is required"))?;

                let mut fields: Vec<(String, String)> = Vec::new();
                for col in entity
                    .columns
                    .iter()
                    .filter(|c| c.graphql && c.name != entity.id_column)
                {
                    if let Some(acc) = input.get(col.name.as_str()) {
                        let val = acc.deserialize::<GqlValue>().unwrap_or(GqlValue::Null);
                        fields.push((col.name.clone(), gql_to_sql_literal(&val, &col.pg_type)));
                    }
                }
                if fields.is_empty() {
                    return Err(async_graphql::Error::new(
                        "update input must contain at least one field",
                    ));
                }

                let id_literal = format!("'{}'", id.replace('\'', "''"));
                let returning: Vec<&str> = entity.columns.iter().map(|c| c.name.as_str()).collect();
                let row = db::update_row_rls(
                    &pool,
                    &entity.table,
                    &entity.id_column,
                    &id_literal,
                    &fields,
                    &returning,
                    &rls_params,
                )
                .await?;

                if let Some(row) = row {
                    sync_to_es(&pool, &es, &entity, &id, row.clone(), &cfg).await;
                    Ok(Some(row_to_gql(row)))
                } else {
                    Ok(None)
                }
            })
        },
    )
    .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::STRING)))
    .argument(InputValue::new("input", TypeRef::named_nn(&update_tn)));

    // ── delete_<entity> ───────────────────────────────────────────────────
    let entity_d = entity.clone();
    let pool_d = Arc::clone(&pool);
    let es_d = Arc::clone(&es);
    let oauth2_d = oauth2_cfg.clone();

    let delete_field = Field::new(
        format!("delete_{}", snake_pub(&entity.name)),
        TypeRef::named_nn(TypeRef::BOOLEAN),
        move |ctx| {
            let pool = Arc::clone(&pool_d);
            let es = Arc::clone(&es_d);
            let entity = entity_d.clone();
            let oauth2_cfg = oauth2_d.clone();
            FieldFuture::new(async move {
                let rls_params = ctx
                    .data::<AuthContext>()
                    .ok()
                    .zip(oauth2_cfg.as_deref())
                    .map(|(a, c)| a.rls_params(c))
                    .unwrap_or_default();
                let id: String = ctx
                    .args
                    .get("id")
                    .and_then(|v| v.string().ok().map(str::to_owned))
                    .ok_or_else(|| async_graphql::Error::new("id is required"))?;

                let id_literal = format!("'{}'", id.replace('\'', "''"));
                let deleted = db::delete_row_rls(
                    &pool,
                    &entity.table,
                    &entity.id_column,
                    &id_literal,
                    &rls_params,
                )
                .await?;

                if deleted {
                    if let Err(e) = es.delete_document(&entity.index, &id).await {
                        tracing::warn!("[{}] ES delete failed for {id}: {e}", entity.index);
                    }
                }
                Ok(Some(GqlValue::Boolean(deleted)))
            })
        },
    )
    .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::STRING)));

    (create_field, update_field, delete_field)
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Convert a `GqlValue` (async-graphql runtime value) to a SQL literal.
fn gql_to_sql_literal(v: &GqlValue, pg_type: &PgType) -> String {
    match v {
        GqlValue::Null => "NULL".to_string(),
        GqlValue::Boolean(b) => b.to_string(),
        GqlValue::Number(n) => n.to_string(),
        GqlValue::String(s) => {
            let e = s.replace('\'', "''");
            match pg_type {
                PgType::Uuid => format!("'{e}'::uuid"),
                PgType::Timestamptz => format!("'{e}'::timestamptz"),
                PgType::Timestamp => format!("'{e}'::timestamp"),
                PgType::Date => format!("'{e}'::date"),
                PgType::Jsonb => format!("'{e}'::jsonb"),
                PgType::Json => format!("'{e}'::json"),
                PgType::Numeric => format!("'{e}'::numeric"),
                _ => format!("'{e}'"),
            }
        }
        other => {
            // Nested object/list — serialise as JSONB
            let json = serde_json::to_string(&gql_to_json(other)).unwrap_or_default();
            format!("'{}'::jsonb", json.replace('\'', "''"))
        }
    }
}

fn gql_to_json(v: &GqlValue) -> serde_json::Value {
    match v {
        GqlValue::Null => serde_json::Value::Null,
        GqlValue::Boolean(b) => serde_json::Value::Bool(*b),
        GqlValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::json!(i)
            } else if let Some(f) = n.as_f64() {
                serde_json::json!(f)
            } else {
                serde_json::Value::Null
            }
        }
        GqlValue::String(s) => serde_json::Value::String(s.clone()),
        GqlValue::List(a) => serde_json::Value::Array(a.iter().map(gql_to_json).collect()),
        GqlValue::Object(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| (k.to_string(), gql_to_json(v)))
                .collect(),
        ),
        _ => serde_json::Value::Null,
    }
}

fn extract_id(row: &std::collections::HashMap<String, serde_json::Value>, id_col: &str) -> String {
    match row.get(id_col) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string().trim_matches('"').to_owned(),
        None => String::new(),
    }
}

/// Rebuild search_text then upsert the ES document.
/// Fire-and-forget — logs on failure, does not bubble up to the mutation caller.
async fn sync_to_es(
    pool: &PgPool,
    es: &EsClient,
    entity: &EntityConfig,
    id: &str,
    mut row: std::collections::HashMap<String, serde_json::Value>,
    cfg: &Config,
) {
    if entity.search_text.is_some() {
        match indexer::build_search_text_for_id(pool, entity, id, cfg).await {
            Ok(Some(text)) => {
                let field = entity
                    .search_text
                    .as_ref()
                    .map(|c| c.field.as_str())
                    .unwrap_or("search_text");
                row.insert(field.to_owned(), serde_json::Value::String(text));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("[{}] search_text rebuild failed: {e}", entity.index),
        }
    }
    if let Err(e) = es
        .put_document(&entity.index, id, serde_json::json!(row))
        .await
    {
        tracing::warn!("[{}] ES upsert failed for {id}: {e}", entity.index);
    }
}
