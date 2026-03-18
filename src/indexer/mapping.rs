use crate::config::{ColumnConfig, EsFieldType, PgType};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Derive the ES field type from a Postgres column config.
pub fn derive_es_type(col: &ColumnConfig) -> EsFieldType {
    if let Some(ref explicit) = col.es_type {
        return explicit.clone();
    }
    match col.pg_type {
        PgType::Uuid                           => EsFieldType::Keyword,
        PgType::Text | PgType::Varchar         => EsFieldType::Text,
        PgType::Int2 | PgType::Int4            => EsFieldType::Integer,
        PgType::Int8                           => EsFieldType::Long,
        PgType::Float4                         => EsFieldType::Float,
        PgType::Float8                         => EsFieldType::Double,
        PgType::Numeric                        => EsFieldType::ScaledFloat,
        PgType::Bool                           => EsFieldType::Boolean,
        PgType::Timestamptz | PgType::Timestamp => EsFieldType::Date,
        PgType::Date                           => EsFieldType::Date,
        PgType::Jsonb | PgType::Json           => EsFieldType::Object,
        PgType::Other                          => EsFieldType::Keyword,
    }
}

/// Build the ES `mappings` object for an index.
pub fn build_mappings(columns: &[ColumnConfig]) -> Value {
    let mut properties: HashMap<String, Value> = HashMap::new();

    for col in columns {
        if !col.indexed {
            continue;
        }
        let es_type = derive_es_type(col);
        let mut field_def = build_field_def(&es_type, col);

        // Add .keyword sub-field for text columns
        if es_type == EsFieldType::Text && col.keyword_subfield {
            field_def["fields"] = json!({
                "keyword": { "type": "keyword", "ignore_above": 256 }
            });
        }

        // Merge any extra user-supplied ES properties
        if !col.es_extra.is_empty() {
            for (k, v) in &col.es_extra {
                field_def[k] = v.clone();
            }
        }

        properties.insert(col.name.clone(), field_def);
    }

    json!({ "properties": properties })
}

fn build_field_def(es_type: &EsFieldType, _col: &ColumnConfig) -> Value {
    match es_type {
        EsFieldType::ScaledFloat => json!({
            "type": "scaled_float",
            "scaling_factor": 100
        }),
        EsFieldType::Date => json!({
            "type": "date",
            "format": "strict_date_optional_time||epoch_millis"
        }),
        _ => json!({ "type": es_type.to_string() }),
    }
}

/// Build a full index create body including settings + mappings.
/// If `search_text_field` is Some, adds a `text` field for the denormalized search string.
pub fn build_index_body(
    columns:           &[ColumnConfig],
    number_of_shards:  u32,
    number_of_replicas: u32,
    search_text_field: Option<&str>,
) -> Value {
    let mut mappings = build_mappings(columns);
    // Inject the search_text field as a dedicated `text` mapping
    if let Some(field) = search_text_field {
        mappings["properties"][field] = json!({
            "type": "text",
            "analyzer": "standard"
        });
    }
    json!({
        "settings": {
            "number_of_shards": number_of_shards,
            "number_of_replicas": number_of_replicas
        },
        "mappings": mappings
    })
}
