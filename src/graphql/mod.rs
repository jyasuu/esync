use crate::config::{Config, EntityConfig, PgType};
use anyhow::Result;
use async_graphql::dynamic::*;
use async_graphql::{Name, Value as GqlValue};
use sqlx::PgPool;
use std::sync::Arc;

/// Build a fully dynamic GraphQL schema from the entity configs.
/// Each entity gets:
///   - `list_<entity>(limit, offset, search)` — paginated list with ILIKE search
///   - `get_<entity>(id!)`                    — single record by primary key
pub fn build_schema(cfg: &Config, pool: Arc<PgPool>) -> Result<Schema> {
    let mut query = Object::new("Query");
    let mut schema_builder = Schema::build("Query", None, None);

    for entity in &cfg.entities {
        let entity    = entity.clone();
        let pool_list = Arc::clone(&pool);
        let pool_get  = Arc::clone(&pool);

        // ── Object type ───────────────────────────────────────────────────
        let mut obj = Object::new(&entity.name);
        for col in &entity.columns {
            if !col.graphql { continue; }
            let gql_type = pg_to_gql_type(&col.pg_type);
            let col_name = col.name.clone();
            obj = obj.field(Field::new(col_name.clone(), gql_type, move |ctx| {
                let name = col_name.clone();
                FieldFuture::new(async move {
                    // async-graphql v7: Value has no as_object() method;
                    // pattern-match on the Object variant directly.
                    let val = match ctx.parent_value.as_value() {
                        Some(GqlValue::Object(map)) => map
                            .get(name.as_str())
                            .cloned()
                            .unwrap_or(GqlValue::Null),
                        _ => GqlValue::Null,
                    };
                    Ok(Some(val))
                })
            }));
        }
        schema_builder = schema_builder.register(obj);

        // ── list_<entity> ─────────────────────────────────────────────────
        let list_name  = format!("list_{}", snake(&entity.name));
        let entity_c   = entity.clone();
        let list_field = Field::new(
            list_name,
            TypeRef::named_nn_list_nn(&entity.name),
            move |ctx| {
                let pool   = Arc::clone(&pool_list);
                let entity = entity_c.clone();
                FieldFuture::new(async move {
                    // ValueAccessor::i64() / string() return Result in async-graphql v7;
                    // use .ok() to convert to Option, falling back to defaults.
                    let limit: i64  = ctx.args.get("limit")
                        .and_then(|v| v.i64().ok())
                        .unwrap_or(20);
                    let offset: i64 = ctx.args.get("offset")
                        .and_then(|v| v.i64().ok())
                        .unwrap_or(0);
                    let search: Option<String> = ctx.args.get("search")
                        .and_then(|v| v.string().ok().map(str::to_owned));

                    let cols: Vec<&str> = entity.columns.iter()
                        .filter(|c| c.graphql)
                        .map(|c| c.name.as_str())
                        .collect();

                    let filter = build_filter(&entity, search.as_deref());
                    let rows   = crate::db::fetch_rows(
                        &pool, &entity.table, &cols,
                        filter.as_deref(), limit, offset,
                    ).await?;

                    let items: Vec<GqlValue> = rows.into_iter()
                        .map(|row| {
                            let map: indexmap::IndexMap<Name, GqlValue> = row
                                .into_iter()
                                .map(|(k, v)| (Name::new(k), json_to_gql(v)))
                                .collect();
                            GqlValue::Object(map)
                        })
                        .collect();

                    Ok(Some(GqlValue::List(items)))
                })
            },
        )
        .argument(InputValue::new("limit",  TypeRef::named(TypeRef::INT)))
        .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT)))
        .argument(InputValue::new("search", TypeRef::named(TypeRef::STRING)));

        query = query.field(list_field);

        // ── get_<entity> ──────────────────────────────────────────────────
        let get_name  = format!("get_{}", snake(&entity.name));
        let entity_g  = entity.clone();
        let get_field = Field::new(
            get_name,
            TypeRef::named(&entity.name),
            move |ctx| {
                let pool   = Arc::clone(&pool_get);
                let entity = entity_g.clone();
                FieldFuture::new(async move {
                    // string() returns Result<&str, Error> in v7 — use .ok()
                    let id: String = ctx.args.get("id")
                        .and_then(|v| v.string().ok().map(str::to_owned))
                        .ok_or_else(|| async_graphql::Error::new("id is required"))?;

                    let cols: Vec<&str> = entity.columns.iter()
                        .filter(|c| c.graphql)
                        .map(|c| c.name.as_str())
                        .collect();

                    let filter = format!(
                        "{} = '{}'",
                        entity.id_column,
                        id.replace('\'', "''")
                    );
                    let mut rows = crate::db::fetch_rows(
                        &pool, &entity.table, &cols, Some(&filter), 1, 0,
                    ).await?;

                    Ok(rows.pop().map(|row| {
                        let map: indexmap::IndexMap<Name, GqlValue> = row
                            .into_iter()
                            .map(|(k, v)| (Name::new(k), json_to_gql(v)))
                            .collect();
                        GqlValue::Object(map)
                    }))
                })
            },
        )
        .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::STRING)));

        query = query.field(get_field);
    }

    Ok(schema_builder.register(query).finish()?)
}

// ── helpers ───────────────────────────────────────────────────────────────

fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i != 0 { out.push('_'); }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn pg_to_gql_type(pg: &PgType) -> TypeRef {
    match pg {
        PgType::Bool                => TypeRef::named(TypeRef::BOOLEAN),
        PgType::Int2 | PgType::Int4 => TypeRef::named(TypeRef::INT),
        PgType::Int8 | PgType::Numeric | PgType::Float4 | PgType::Float8
                                    => TypeRef::named(TypeRef::FLOAT),
        _                           => TypeRef::named(TypeRef::STRING),
    }
}

fn build_filter(entity: &EntityConfig, search: Option<&str>) -> Option<String> {
    let base = entity.filter.clone();
    let search_clause = search.and_then(|q| {
        let text_cols: Vec<String> = entity.columns.iter()
            .filter(|c| matches!(c.pg_type, PgType::Text | PgType::Varchar))
            .map(|c| format!("{} ILIKE '%{}%'", c.name, q.replace('\'', "''")))
            .collect();
        if text_cols.is_empty() { None } else { Some(format!("({})", text_cols.join(" OR "))) }
    });

    match (base, search_clause) {
        (Some(b), Some(s)) => Some(format!("({b}) AND {s}")),
        (Some(b), None)    => Some(b),
        (None,    Some(s)) => Some(s),
        (None,    None)    => None,
    }
}

fn json_to_gql(v: serde_json::Value) -> GqlValue {
    match v {
        serde_json::Value::Null        => GqlValue::Null,
        serde_json::Value::Bool(b)     => GqlValue::Boolean(b),
        serde_json::Value::Number(n)   => {
            if let Some(i) = n.as_i64() {
                GqlValue::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                GqlValue::Number(
                    async_graphql::Number::from_f64(f).unwrap_or_else(|| 0i32.into())
                )
            } else {
                GqlValue::Null
            }
        }
        serde_json::Value::String(s)   => GqlValue::String(s),
        serde_json::Value::Array(arr)  => GqlValue::List(arr.into_iter().map(json_to_gql).collect()),
        serde_json::Value::Object(obj) => GqlValue::Object(
            obj.into_iter()
               .map(|(k, v)| (Name::new(k), json_to_gql(v)))
               .collect::<indexmap::IndexMap<Name, GqlValue>>()
        ),
    }
}
