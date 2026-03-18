/// tests/test_mapping.rs
/// Unit tests for src/indexer/mapping.rs.
/// No Postgres or Elasticsearch required.

use esync::{
    config::{ColumnConfig, EsFieldType, PgType},
    indexer::mapping::{build_index_body, build_mappings, derive_es_type},
};
use serde_json::json;
use std::collections::HashMap;

// ── Helpers ───────────────────────────────────────────────────────────────

fn col(name: &str, pg_type: PgType) -> ColumnConfig {
    ColumnConfig {
        name:            name.to_string(),
        pg_type,
        es_type:         None,
        keyword_subfield: true,
        graphql:         true,
        indexed:         true,
        es_extra:        HashMap::new(),
    }
}

fn col_override(name: &str, pg_type: PgType, es_type: EsFieldType) -> ColumnConfig {
    ColumnConfig { es_type: Some(es_type), ..col(name, pg_type) }
}

fn col_no_kw(name: &str, pg_type: PgType) -> ColumnConfig {
    ColumnConfig { keyword_subfield: false, ..col(name, pg_type) }
}

fn col_no_index(name: &str, pg_type: PgType) -> ColumnConfig {
    ColumnConfig { indexed: false, ..col(name, pg_type) }
}

fn col_extra(name: &str, pg_type: PgType, key: &str, val: serde_json::Value) -> ColumnConfig {
    let mut extra = HashMap::new();
    extra.insert(key.to_string(), val);
    ColumnConfig { es_extra: extra, ..col(name, pg_type) }
}

// ── derive_es_type ────────────────────────────────────────────────────────

#[test] fn uuid_to_keyword()        { assert_eq!(derive_es_type(&col("id", PgType::Uuid)),       EsFieldType::Keyword);     }
#[test] fn text_to_text()           { assert_eq!(derive_es_type(&col("n",  PgType::Text)),        EsFieldType::Text);        }
#[test] fn varchar_to_text()        { assert_eq!(derive_es_type(&col("s",  PgType::Varchar)),     EsFieldType::Text);        }
#[test] fn int4_to_integer()        { assert_eq!(derive_es_type(&col("i",  PgType::Int4)),        EsFieldType::Integer);     }
#[test] fn int2_to_integer()        { assert_eq!(derive_es_type(&col("i",  PgType::Int2)),        EsFieldType::Integer);     }
#[test] fn int8_to_long()           { assert_eq!(derive_es_type(&col("i",  PgType::Int8)),        EsFieldType::Long);        }
#[test] fn float4_to_float()        { assert_eq!(derive_es_type(&col("f",  PgType::Float4)),      EsFieldType::Float);       }
#[test] fn float8_to_double()       { assert_eq!(derive_es_type(&col("f",  PgType::Float8)),      EsFieldType::Double);      }
#[test] fn numeric_to_scaled()      { assert_eq!(derive_es_type(&col("p",  PgType::Numeric)),     EsFieldType::ScaledFloat); }
#[test] fn bool_to_boolean()        { assert_eq!(derive_es_type(&col("b",  PgType::Bool)),        EsFieldType::Boolean);     }
#[test] fn timestamptz_to_date()    { assert_eq!(derive_es_type(&col("t",  PgType::Timestamptz)), EsFieldType::Date);        }
#[test] fn timestamp_to_date()      { assert_eq!(derive_es_type(&col("t",  PgType::Timestamp)),   EsFieldType::Date);        }
#[test] fn date_to_date()           { assert_eq!(derive_es_type(&col("d",  PgType::Date)),        EsFieldType::Date);        }
#[test] fn jsonb_to_object()        { assert_eq!(derive_es_type(&col("j",  PgType::Jsonb)),       EsFieldType::Object);      }
#[test] fn json_to_object()         { assert_eq!(derive_es_type(&col("j",  PgType::Json)),        EsFieldType::Object);      }

#[test]
fn explicit_es_type_overrides_pg_type() {
    // TEXT would normally → text, but we force keyword
    let c = col_override("tag", PgType::Text, EsFieldType::Keyword);
    assert_eq!(derive_es_type(&c), EsFieldType::Keyword);
}

// ── build_mappings ────────────────────────────────────────────────────────

#[test]
fn text_field_gets_keyword_subfield() {
    let m = build_mappings(&[col("name", PgType::Text)]);
    assert_eq!(m["properties"]["name"]["type"], "text");
    assert_eq!(m["properties"]["name"]["fields"]["keyword"]["type"], "keyword");
}

#[test]
fn text_field_without_subfield_when_disabled() {
    let m = build_mappings(&[col_no_kw("body", PgType::Text)]);
    assert_eq!(m["properties"]["body"]["type"], "text");
    assert!(m["properties"]["body"].get("fields").is_none());
}

#[test]
fn keyword_field_has_no_subfield() {
    let m = build_mappings(&[col("id", PgType::Uuid)]);
    assert_eq!(m["properties"]["id"]["type"], "keyword");
    // UUID → keyword; should not have a .keyword sub-field of its own
    let no_kw = m["properties"]["id"].get("fields").is_none()
        || m["properties"]["id"]["fields"].is_null();
    assert!(no_kw, "keyword fields must not get .keyword sub-field");
}

#[test]
fn date_field_has_format_string() {
    let m = build_mappings(&[col("ts", PgType::Timestamptz)]);
    assert_eq!(m["properties"]["ts"]["type"], "date");
    let fmt = m["properties"]["ts"]["format"].as_str().unwrap_or("");
    assert!(fmt.contains("strict_date_optional_time"), "date format string missing");
}

#[test]
fn scaled_float_has_scaling_factor_100() {
    let m = build_mappings(&[col("price", PgType::Numeric)]);
    assert_eq!(m["properties"]["price"]["type"],           "scaled_float");
    assert_eq!(m["properties"]["price"]["scaling_factor"], json!(100));
}

#[test]
fn indexed_false_excludes_field() {
    let cols = vec![col("id", PgType::Uuid), col_no_index("secret", PgType::Text)];
    let m    = build_mappings(&cols);
    assert!(m["properties"].get("id").is_some(),      "id should be in mappings");
    assert!(m["properties"].get("secret").is_none(),  "non-indexed field must be absent");
}

#[test]
fn es_extra_is_merged_into_field() {
    let m = build_mappings(&[col_extra("desc", PgType::Text, "copy_to", json!("_all"))]);
    assert_eq!(m["properties"]["desc"]["copy_to"], json!("_all"));
}

#[test]
fn multiple_fields_all_appear() {
    let cols = vec![
        col("id",    PgType::Uuid),
        col("name",  PgType::Text),
        col("price", PgType::Numeric),
        col("ts",    PgType::Timestamptz),
        col("meta",  PgType::Jsonb),
    ];
    let m = build_mappings(&cols);
    for name in ["id", "name", "price", "ts", "meta"] {
        assert!(m["properties"].get(name).is_some(), "Field {name} missing from mappings");
    }
}

// ── build_index_body ──────────────────────────────────────────────────────

#[test]
fn index_body_has_settings_and_mappings() {
    let body = build_index_body(&[col("id", PgType::Uuid)], 2, 1, None);
    assert_eq!(body["settings"]["number_of_shards"],   json!(2));
    assert_eq!(body["settings"]["number_of_replicas"], json!(1));
    assert!(body["mappings"]["properties"].is_object());
}
