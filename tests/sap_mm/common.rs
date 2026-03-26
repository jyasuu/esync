//! tests/sap_mm/common.rs
//! Shared helpers for the SAP MM integration test suite.
#![allow(dead_code)]

use anyhow::Result;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::time::Duration;
use tokio::time::sleep;

// ── Connection constants (match esync-sap-mm.test.yaml) ──────────────────────

pub const PG_URL: &str = "postgres://esync:esync@localhost:5432/esync_mm_test";
pub const ES_URL: &str = "http://localhost:9200";
pub const GQL_URL: &str = "http://127.0.0.1:4002/graphql";
pub const CFG_PATH: &str = "examples/sap-mm/esync-sap-mm.test.yaml";

// ── Fixed UUIDs from setup_test_db.sql ───────────────────────────────────────

pub const VENDOR_ACME: &str = "aa000000-0000-0000-0000-000000000001";
pub const VENDOR_GLOBAL: &str = "aa000000-0000-0000-0000-000000000002";
pub const VENDOR_PACIFIC: &str = "aa000000-0000-0000-0000-000000000003";

pub const MAT_STEEL: &str = "bb000000-0000-0000-0000-000000000001"; // MAT-1000
pub const MAT_BOLT: &str = "bb000000-0000-0000-0000-000000000002"; // MAT-1001
pub const MAT_PUMP: &str = "bb000000-0000-0000-0000-000000000003"; // MAT-2000
pub const MAT_CONTROL: &str = "bb000000-0000-0000-0000-000000000004"; // MAT-3000
pub const MAT_INACT: &str = "bb000000-0000-0000-0000-000000000005"; // MAT-INACT (deleted_at set)

// ── ES test index names ───────────────────────────────────────────────────────

pub const IDX_MATERIAL: &str = "test_mm_material";
pub const IDX_PLANT: &str = "test_mm_plant_data";
pub const IDX_STOCK: &str = "test_mm_stock";
pub const IDX_VENDOR: &str = "test_mm_vendor";
pub const IDX_PURCH_INFO: &str = "test_mm_purchasing_info";
pub const IDX_MATERIAL_DOC: &str = "test_mm_material_doc";

// ── Postgres helpers ──────────────────────────────────────────────────────────

pub async fn pg_pool() -> Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(5)
        .connect(PG_URL)
        .await?)
}

/// Re-seed all MM tables to a clean, deterministic state.
pub async fn reseed(pool: &PgPool) -> Result<()> {
    sqlx::query("CALL seed_mm_test_data()")
        .execute(pool)
        .await?;
    Ok(())
}

// ── Elasticsearch helpers ─────────────────────────────────────────────────────

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

pub async fn es_get(index: &str, id: &str) -> Result<Option<Value>> {
    let resp = http()
        .get(format!("{ES_URL}/{index}/_doc/{id}"))
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    let body: Value = resp.json().await?;
    if body["found"].as_bool() == Some(false) {
        return Ok(None);
    }
    Ok(body.get("_source").cloned())
}

pub async fn es_search(index: &str, query: &str) -> Result<Vec<Value>> {
    let resp = http()
        .post(format!("{ES_URL}/{index}/_search"))
        .json(&serde_json::json!({
            "query": { "multi_match": { "query": query, "fields": ["*"] } },
            "size": 100
        }))
        .send()
        .await?;
    let data: Value = resp.json().await?;
    Ok(data["hits"]["hits"].as_array().cloned().unwrap_or_default())
}

pub async fn es_all(index: &str) -> Result<Vec<Value>> {
    let resp = http()
        .post(format!("{ES_URL}/{index}/_search"))
        .json(&serde_json::json!({ "query": { "match_all": {} }, "size": 1000 }))
        .send()
        .await?;
    let data: Value = resp.json().await?;
    Ok(data["hits"]["hits"].as_array().cloned().unwrap_or_default())
}

pub async fn es_delete_index(index: &str) -> Result<()> {
    let _ = http().delete(format!("{ES_URL}/{index}")).send().await;
    Ok(())
}

pub async fn es_refresh(index: &str) -> Result<()> {
    http()
        .post(format!("{ES_URL}/{index}/_refresh"))
        .send()
        .await?;
    Ok(())
}

pub async fn es_count(index: &str) -> Result<u64> {
    let resp = http()
        .get(format!("{ES_URL}/{index}/_count"))
        .send()
        .await?;
    let data: Value = resp.json().await?;
    Ok(data["count"].as_u64().unwrap_or(0))
}

// ── Polling helper ────────────────────────────────────────────────────────────

pub async fn wait_until<F, Fut>(timeout: Duration, interval: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        sleep(interval).await;
    }
}

// ── GraphQL helpers ───────────────────────────────────────────────────────────

pub async fn gql(query: &str, variables: Option<Value>) -> Result<Value> {
    let body = match variables {
        Some(v) => serde_json::json!({ "query": query, "variables": v }),
        None => serde_json::json!({ "query": query }),
    };
    let resp = http().post(GQL_URL).json(&body).send().await?;
    Ok(resp.json().await?)
}

pub fn assert_no_gql_errors(resp: &Value) {
    if let Some(errs) = resp.get("errors") {
        if !errs.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            panic!("GraphQL errors:\n{errs:#}");
        }
    }
}

/// Parse a numeric-ish JSON value that may come back as a number or a string
/// (ES stores NUMERIC fields as strings via row_to_json in some paths).
pub fn parse_numeric(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}
