//! ES-backed search field builder.
//!
//! For each entity with `search.enabled: true` this module generates a
//! `search_<entity>` GraphQL query that:
//!   1. Runs a multi_match / bool query against Elasticsearch
//!   2. Optionally re-fetches live columns from Postgres (bypasses ES staleness)
//!   3. Resolves configured `enrich` relations from Postgres in one query per relation
//!   4. Returns a `<Entity>SearchResult` wrapper carrying score + highlight + data

use crate::{
    config::{Config, EntityConfig, RelationConfig, RelationKind},
    elastic::EsClient,
};
use anyhow::Result;
use async_graphql::dynamic::*;
use async_graphql::{Name, Value as GqlValue};
use serde_json::{json, Value as JsValue};
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};

// ── Public entry point ────────────────────────────────────────────────────

/// Register all `search_*` fields and their supporting types onto `builder`
/// and `query` object for every entity that has `search.enabled: true`.
pub fn register_search(
    cfg: &Config,
    pool: Arc<PgPool>,
    es: Arc<EsClient>,
    builder: SchemaBuilder,
    query: Object,
) -> (SchemaBuilder, Object) {
    let mut builder = builder;
    let mut query = query;

    for entity in &cfg.entities {
        if !entity.search.enabled {
            continue;
        }

        // Register the SearchResult wrapper type for this entity
        let result_type = build_result_type(entity);
        builder = builder.register(result_type);

        // Register the SearchPage type (total + items)
        let page_type = build_page_type(entity);
        builder = builder.register(page_type);

        // Register the Highlight type for this entity
        let hl_type = build_highlight_type(entity);
        builder = builder.register(hl_type);

        // Add the search_<entity> field to Query
        let field = build_search_field(
            entity,
            Arc::clone(&cfg_arc(cfg)),
            Arc::clone(&pool),
            Arc::clone(&es),
        );
        query = query.field(field);
    }

    (builder, query)
}

// Helper: wrap Config in Arc without requiring it to be passed as Arc
fn cfg_arc(cfg: &Config) -> Arc<Config> {
    Arc::new(cfg.clone())
}

// ── Type names ────────────────────────────────────────────────────────────

fn result_type_name(entity: &EntityConfig) -> String {
    format!("{}SearchResult", entity.name)
}
fn page_type_name(entity: &EntityConfig) -> String {
    format!("{}SearchPage", entity.name)
}
fn highlight_type_name(entity: &EntityConfig) -> String {
    format!("{}Highlight", entity.name)
}

// ── Dynamic type builders ─────────────────────────────────────────────────

/// `<Entity>Highlight { <field>: String ... }`
fn build_highlight_type(entity: &EntityConfig) -> Object {
    let mut obj = Object::new(highlight_type_name(entity));
    for field_name in &entity.search.highlight {
        let fname = field_name.clone();
        obj = obj.field(Field::new(
            fname.clone(),
            TypeRef::named(TypeRef::STRING),
            move |ctx| {
                let name = fname.clone();
                FieldFuture::new(async move { Ok(Some(extract_field(&ctx, &name))) })
            },
        ));
    }
    // Always expose a raw `_all` field with the full highlight map as JSON string
    obj = obj.field(Field::new("_all", TypeRef::named(TypeRef::STRING), |ctx| {
        FieldFuture::new(async move {
            let val = match ctx.parent_value.as_value() {
                Some(GqlValue::Object(m)) => {
                    let json_map: serde_json::Map<String, JsValue> = m
                        .iter()
                        .filter_map(|(k, v)| gql_to_json(v.clone()).map(|j| (k.to_string(), j)))
                        .collect();
                    GqlValue::String(serde_json::to_string(&json_map).unwrap_or_default())
                }
                _ => GqlValue::Null,
            };
            Ok(Some(val))
        })
    }));
    obj
}

/// `<Entity>SearchResult { _score _highlight <all entity scalar fields> <relation fields> }`
fn build_result_type(entity: &EntityConfig) -> Object {
    let mut obj = Object::new(result_type_name(entity));

    // _score
    obj = obj.field(Field::new(
        "_score",
        TypeRef::named(TypeRef::FLOAT),
        |ctx| FieldFuture::new(async move { Ok(Some(extract_field(&ctx, "_score"))) }),
    ));

    // _id (ES document id)
    obj = obj.field(Field::new("_id", TypeRef::named(TypeRef::STRING), |ctx| {
        FieldFuture::new(async move { Ok(Some(extract_field(&ctx, "_id"))) })
    }));

    // _highlight
    let hl_name = highlight_type_name(entity);
    obj = obj.field(Field::new("_highlight", TypeRef::named(&hl_name), |ctx| {
        FieldFuture::new(async move { Ok(Some(extract_field(&ctx, "_highlight"))) })
    }));

    // All scalar columns from the entity (sourced from ES _source, overridden by live PG values)
    for col in &entity.columns {
        if !col.graphql {
            continue;
        }
        let col_name = col.name.clone();
        let gql_type = super::pg_to_gql_type_pub(&col.pg_type);
        obj = obj.field(Field::new(col_name.clone(), gql_type, move |ctx| {
            let name = col_name.clone();
            FieldFuture::new(async move { Ok(Some(extract_field(&ctx, &name))) })
        }));
    }

    // Relation fields (same pattern as in the main object type)
    for rel in &entity.relations {
        let rel_type = match rel.kind {
            RelationKind::BelongsTo => TypeRef::named(&rel.target),
            RelationKind::HasMany | RelationKind::ManyToMany => {
                TypeRef::named_nn_list_nn(&rel.target)
            }
        };
        let rel_name = rel.field.clone();
        obj = obj.field(Field::new(rel_name.clone(), rel_type, move |ctx| {
            let name = rel_name.clone();
            FieldFuture::new(async move { Ok(Some(extract_field(&ctx, &name))) })
        }));
    }

    obj
}

/// `<Entity>SearchPage { total took items: [<Entity>SearchResult!]! }`
fn build_page_type(entity: &EntityConfig) -> Object {
    let result_name = result_type_name(entity);
    let mut obj = Object::new(page_type_name(entity));

    obj = obj.field(Field::new("total", TypeRef::named(TypeRef::INT), |ctx| {
        FieldFuture::new(async move { Ok(Some(extract_field(&ctx, "total"))) })
    }));
    obj = obj.field(Field::new("took", TypeRef::named(TypeRef::INT), |ctx| {
        FieldFuture::new(async move { Ok(Some(extract_field(&ctx, "took"))) })
    }));
    obj = obj.field(Field::new(
        "items",
        TypeRef::named_nn_list_nn(&result_name),
        |ctx| FieldFuture::new(async move { Ok(Some(extract_field(&ctx, "items"))) }),
    ));
    obj
}

// ── search_<entity> field ─────────────────────────────────────────────────

fn build_search_field(
    entity: &EntityConfig,
    cfg: Arc<Config>,
    pool: Arc<PgPool>,
    es: Arc<EsClient>,
) -> Field {
    let page_name = page_type_name(entity);
    let field_name = format!("search_{}", super::snake_pub(&entity.name));
    let entity_c = entity.clone();

    Field::new(field_name, TypeRef::named_nn(&page_name), move |ctx| {
        let entity = entity_c.clone();
        let cfg = Arc::clone(&cfg);
        let pool = Arc::clone(&pool);
        let es = Arc::clone(&es);

        FieldFuture::new(async move {
            // ── 1. Read GQL arguments ────────────────────────────────────
            let q: Option<String> = ctx
                .args
                .get("q")
                .and_then(|v| v.string().ok().map(str::to_owned));
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
            let sort_arg: Option<String> = ctx
                .args
                .get("sort")
                .and_then(|v| v.string().ok().map(str::to_owned));
            let filter_arg: Option<String> = ctx
                .args
                .get("filter")
                .and_then(|v| v.string().ok().map(str::to_owned));

            // ── 2. Build ES query ────────────────────────────────────────
            let es_body = build_es_query(
                &entity,
                q.as_deref(),
                filter_arg.as_deref(),
                sort_arg.as_deref(),
                limit,
                offset,
            );

            // ── 3. Determine indices (own + cross_index) ─────────────────
            let mut indices = vec![entity.index.clone()];
            for extra_name in &entity.search.cross_index {
                if let Some(e) = cfg.entity(extra_name) {
                    indices.push(e.index.clone());
                }
            }
            let index_str = indices.join(",");

            // ── 4. Execute ES search ─────────────────────────────────────
            let es_resp = es.search(&index_str, es_body).await?;

            let total = es_resp["hits"]["total"]["value"].as_i64().unwrap_or(0);
            let took = es_resp["took"].as_i64().unwrap_or(0);
            let hits = es_resp["hits"]["hits"]
                .as_array()
                .cloned()
                .unwrap_or_default();

            // ── 5. Extract ids + build per-hit base maps ─────────────────
            let mut hit_maps: Vec<HashMap<String, serde_json::Value>> = hits
                .iter()
                .map(|hit| {
                    let mut m: HashMap<String, serde_json::Value> = HashMap::new();
                    // Merge _source fields first
                    if let Some(src) = hit["_source"].as_object() {
                        for (k, v) in src {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    // Meta fields
                    m.insert("_score".to_owned(), hit["_score"].clone());
                    m.insert("_id".to_owned(), hit["_id"].clone());
                    // Highlight: build per-field first-fragment map
                    let hl_map: serde_json::Map<String, serde_json::Value> = hit["highlight"]
                        .as_object()
                        .map(|hl| {
                            hl.iter()
                                .map(|(k, v)| {
                                    let snippet = v
                                        .as_array()
                                        .and_then(|a| a.first())
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("")
                                        .to_owned();
                                    (k.clone(), JsValue::String(snippet))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    m.insert("_highlight".to_owned(), JsValue::Object(hl_map));
                    m
                })
                .collect();

            let ids: Vec<String> = hit_maps
                .iter()
                .filter_map(|m| m.get("_id").and_then(|v| v.as_str()).map(str::to_owned))
                .collect();

            // ── 6. Live PG columns (overwrite ES _source values) ─────────
            if !entity.search.live_columns.is_empty() && !ids.is_empty() {
                let live_cols: Vec<&str> = entity
                    .search
                    .live_columns
                    .iter()
                    .map(String::as_str)
                    .collect();
                // include id_column so we can match by it
                let mut fetch_cols = vec![entity.id_column.as_str()];
                fetch_cols.extend_from_slice(&live_cols);
                fetch_cols.dedup();

                let live_rows = crate::db::fetch_by_ids(
                    &pool,
                    &entity.table,
                    &entity.id_column,
                    &fetch_cols,
                    &ids,
                    entity.filter.as_deref(),
                )
                .await?;

                for hit in &mut hit_maps {
                    let id = hit
                        .get("_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned();
                    if let Some(live) = live_rows.get(&id) {
                        for col in &live_cols {
                            if let Some(val) = live.get(*col) {
                                hit.insert(col.to_string(), val.clone());
                            }
                        }
                    }
                }
            }

            // ── 7. Batch-resolve enriched relations ───────────────────────
            for rel_name in &entity.search.enrich {
                let rel = match entity.relations.iter().find(|r| &r.field == rel_name) {
                    Some(r) => r.clone(),
                    None => continue,
                };
                let target_entity = match cfg.entity(&rel.target) {
                    Some(e) => e.clone(),
                    None => continue,
                };

                let target_cols: Vec<&str> = target_entity
                    .columns
                    .iter()
                    .filter(|c| c.graphql)
                    .map(|c| c.name.as_str())
                    .collect();

                match rel.kind {
                    RelationKind::BelongsTo => {
                        // Collect all FK values, batch fetch target rows
                        let fk_vals: Vec<String> = hit_maps
                            .iter()
                            .filter_map(|m| {
                                m.get(&rel.local_col)
                                    .and_then(|v| v.as_str().map(str::to_owned))
                            })
                            .collect();

                        let fetched = crate::db::fetch_by_ids(
                            &pool,
                            &target_entity.table,
                            &rel.foreign_col,
                            &target_cols,
                            &fk_vals,
                            target_entity.filter.as_deref(),
                        )
                        .await?;

                        for hit in &mut hit_maps {
                            let fk = hit
                                .get(&rel.local_col)
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned();
                            let related = fetched
                                .get(&fk)
                                .map(|r| {
                                    JsValue::Object(
                                        r.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                                    )
                                })
                                .unwrap_or(JsValue::Null);
                            hit.insert(rel.field.clone(), related);
                        }
                    }
                    RelationKind::HasMany => {
                        // Collect all local key values, fetch all related rows
                        let local_vals: Vec<String> = hit_maps
                            .iter()
                            .filter_map(|m| {
                                m.get(&rel.local_col)
                                    .and_then(|v| v.as_str().map(str::to_owned))
                            })
                            .collect();

                        let mut fetch_cols = vec![rel.foreign_col.as_str()];
                        fetch_cols.extend(target_cols.iter().copied());
                        fetch_cols.dedup();

                        let order = rel.order_by.as_deref().unwrap_or("1");
                        let rows = fetch_related_rows(
                            &pool,
                            &target_entity,
                            &fetch_cols,
                            &rel.foreign_col,
                            &local_vals,
                            rel.filter.as_deref(),
                            order,
                            rel.limit,
                        )
                        .await?;

                        // Group by FK value
                        let mut groups: HashMap<String, Vec<JsValue>> = HashMap::new();
                        for row in rows {
                            let fk = row
                                .get(&rel.foreign_col)
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned();
                            let entry = groups.entry(fk).or_default();
                            entry.push(JsValue::Object(row.into_iter().collect()));
                        }

                        for hit in &mut hit_maps {
                            let local_val = hit
                                .get(&rel.local_col)
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned();
                            let related = groups.get(&local_val).cloned().unwrap_or_default();
                            hit.insert(rel.field.clone(), JsValue::Array(related));
                        }
                    }
                    RelationKind::ManyToMany => {
                        let join_table = match &rel.join_table {
                            Some(t) => t.clone(),
                            None => continue,
                        };
                        let local_vals: Vec<String> = hit_maps
                            .iter()
                            .filter_map(|m| {
                                m.get(&rel.local_col)
                                    .and_then(|v| v.as_str().map(str::to_owned))
                            })
                            .collect();

                        let rows = fetch_m2m_rows(
                            &pool,
                            &target_entity,
                            &target_cols,
                            &join_table,
                            &rel,
                            &local_vals,
                        )
                        .await?;

                        // Each row carries the join local_col so we can group
                        let mut groups: HashMap<String, Vec<JsValue>> = HashMap::new();
                        for row in rows {
                            let local_key = row
                                .get(&rel.local_col)
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned();
                            groups
                                .entry(local_key)
                                .or_default()
                                .push(JsValue::Object(row.into_iter().collect()));
                        }
                        for hit in &mut hit_maps {
                            let local_val = hit
                                .get(&rel.local_col)
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned();
                            let related = groups.get(&local_val).cloned().unwrap_or_default();
                            hit.insert(rel.field.clone(), JsValue::Array(related));
                        }
                    }
                }
            }

            // ── 8. Convert to GqlValue and return ────────────────────────
            let items: Vec<GqlValue> = hit_maps
                .into_iter()
                .map(|m| {
                    GqlValue::Object(
                        m.into_iter()
                            .map(|(k, v)| (Name::new(k), json_to_gql(v)))
                            .collect::<indexmap::IndexMap<Name, GqlValue>>(),
                    )
                })
                .collect();

            let page = GqlValue::Object(indexmap::indexmap! {
                Name::new("total") => GqlValue::Number((total as i32).into()),
                Name::new("took")  => GqlValue::Number((took  as i32).into()),
                Name::new("items") => GqlValue::List(items),
            });

            Ok(Some(page))
        })
    })
    .argument(InputValue::new("q", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("filter", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("sort", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
    .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT)))
}

// ── ES query builder ──────────────────────────────────────────────────────

fn build_es_query(
    entity: &EntityConfig,
    q: Option<&str>,
    filter_str: Option<&str>,
    sort_str: Option<&str>,
    limit: i64,
    offset: i64,
) -> JsValue {
    // Build multi_match query if q is provided
    let query_clause = if let Some(text) = q.filter(|s| !s.is_empty()) {
        let fields: Vec<JsValue> = entity
            .search
            .fields
            .iter()
            .map(|f| JsValue::String(f.field.clone()))
            .collect();
        let fields = if fields.is_empty() {
            json!(["*"])
        } else {
            JsValue::Array(fields)
        };
        json!({
            "multi_match": {
                "query": text,
                "fields": fields,
                "type": "best_fields",
                "fuzziness": "AUTO",
                "operator": "or"
            }
        })
    } else {
        json!({ "match_all": {} })
    };

    // Parse the filter string as inline ES JSON filter
    // Expected format: JSON string of a single ES filter clause
    // e.g. '{"term":{"active":true}}' or '{"range":{"price":{"gte":10}}}'
    let filter_clause: Option<JsValue> = filter_str.and_then(|f| serde_json::from_str(f).ok());

    let es_query = match filter_clause {
        Some(filter) => json!({
            "bool": {
                "must":   query_clause,
                "filter": filter
            }
        }),
        None => query_clause,
    };

    // Highlight config
    let highlight: Option<JsValue> = if entity.search.highlight.is_empty() {
        None
    } else {
        let fields: serde_json::Map<String, JsValue> = entity
            .search
            .highlight
            .iter()
            .map(|f| {
                (
                    f.clone(),
                    json!({ "number_of_fragments": 1, "fragment_size": 150 }),
                )
            })
            .collect();
        Some(json!({ "fields": fields, "pre_tags": ["<em>"], "post_tags": ["</em>"] }))
    };

    // Sort: default to _score desc, parse override if provided
    let sort: JsValue = sort_str
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!([{ "_score": { "order": "desc" } }]));

    let mut body = json!({
        "query": es_query,
        "sort":  sort,
        "from":  offset,
        "size":  limit,
        "_source": true
    });

    if let Some(hl) = highlight {
        body["highlight"] = hl;
    }

    body
}

// ── Batch SQL helpers ─────────────────────────────────────────────────────

/// Fetch all has_many related rows for a set of FK values in one query.
#[allow(clippy::too_many_arguments)]
async fn fetch_related_rows(
    pool: &PgPool,
    target: &EntityConfig,
    cols: &[&str],
    fk_col: &str,
    fk_vals: &[String],
    extra_filter: Option<&str>,
    order: &str,
    limit: i64,
) -> Result<Vec<HashMap<String, JsValue>>> {
    if fk_vals.is_empty() {
        return Ok(vec![]);
    }
    use sqlx::Row;

    let col_list = cols.join(", ");
    let vals_list = fk_vals
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");

    let mut filter = format!("{fk_col} IN ({vals_list})");
    if let Some(f) = extra_filter {
        filter.push_str(&format!(" AND ({f})"));
    }
    if let Some(f) = &target.filter {
        filter.push_str(&format!(" AND ({f})"));
    }

    let sql = format!(
        "SELECT row_to_json(t)::TEXT AS _row \
         FROM (SELECT {col_list} FROM {} WHERE {filter} \
               ORDER BY {order} LIMIT {limit}) t",
        target.table
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            let txt: String = r.try_get("_row")?;
            let obj: serde_json::Map<String, JsValue> = serde_json::from_str(&txt)?;
            Ok(obj.into_iter().collect())
        })
        .collect()
}

/// Fetch all many_to_many related rows for a set of local key values.
/// Injects the join local_col into each result row so the caller can group.
async fn fetch_m2m_rows(
    pool: &PgPool,
    target: &EntityConfig,
    cols: &[&str],
    join_tbl: &str,
    rel: &RelationConfig,
    local_vals: &[String],
) -> Result<Vec<HashMap<String, JsValue>>> {
    if local_vals.is_empty() {
        return Ok(vec![]);
    }
    use sqlx::Row;

    let col_list = cols
        .iter()
        .map(|c| format!("t.{c}"))
        .collect::<Vec<_>>()
        .join(", ");

    let vals_list = local_vals
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");

    let extra = rel
        .filter
        .as_ref()
        .map(|f| format!(" AND ({f})"))
        .unwrap_or_default();

    let target_extra = target
        .filter
        .as_ref()
        .map(|f| format!(" AND ({f})"))
        .unwrap_or_default();

    // Include join local_col so caller can group results
    let local_col = &rel.local_col;
    let sql = format!(
        "SELECT row_to_json(r)::TEXT AS _row FROM (\
            SELECT {col_list}, j.{local_col} \
            FROM {join_tbl} j \
            JOIN {} t ON t.{} = j.{} \
            WHERE j.{local_col} IN ({vals_list}){extra}{target_extra}\
         ) r",
        target.table, rel.target_id_col, rel.foreign_col
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            let txt: String = r.try_get("_row")?;
            let obj: serde_json::Map<String, JsValue> = serde_json::from_str(&txt)?;
            Ok(obj.into_iter().collect())
        })
        .collect()
}

// ── GQL value helpers ─────────────────────────────────────────────────────

fn extract_field(ctx: &ResolverContext<'_>, name: &str) -> GqlValue {
    match ctx.parent_value.as_value() {
        Some(GqlValue::Object(m)) => m.get(name).cloned().unwrap_or(GqlValue::Null),
        _ => GqlValue::Null,
    }
}

fn json_to_gql(v: JsValue) -> GqlValue {
    match v {
        JsValue::Null => GqlValue::Null,
        JsValue::Bool(b) => GqlValue::Boolean(b),
        JsValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                GqlValue::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                GqlValue::Number(async_graphql::Number::from_f64(f).unwrap_or_else(|| 0i32.into()))
            } else {
                GqlValue::Null
            }
        }
        JsValue::String(s) => GqlValue::String(s),
        JsValue::Array(arr) => GqlValue::List(arr.into_iter().map(json_to_gql).collect()),
        JsValue::Object(obj) => GqlValue::Object(
            obj.into_iter()
                .map(|(k, v)| (Name::new(k), json_to_gql(v)))
                .collect::<indexmap::IndexMap<Name, GqlValue>>(),
        ),
    }
}

fn gql_to_json(v: GqlValue) -> Option<JsValue> {
    Some(match v {
        GqlValue::Null => JsValue::Null,
        GqlValue::Boolean(b) => JsValue::Bool(b),
        GqlValue::Number(n) => serde_json::to_value(n).unwrap_or(JsValue::Null),
        GqlValue::String(s) => JsValue::String(s),
        GqlValue::List(arr) => JsValue::Array(arr.into_iter().filter_map(gql_to_json).collect()),
        GqlValue::Object(m) => JsValue::Object(
            m.into_iter()
                .filter_map(|(k, v)| gql_to_json(v).map(|j| (k.to_string(), j)))
                .collect(),
        ),
        _ => return None,
    })
}
