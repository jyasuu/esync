#![allow(dead_code)]
//! Shared test helpers.
//! Import via:  mod common; use common::*;

use anyhow::Result;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::time::Duration;
use tokio::time::sleep;

// ── Connection constants (match esync.test.yaml) ──────────────────────────

pub const PG_URL: &str = "postgres://esync:esync@localhost:5432/esync_test";
pub const ES_URL: &str = "http://localhost:9200";
pub const GQL_URL: &str = "http://127.0.0.1:4001/graphql";
pub const CFG_PATH: &str = "esync.test.yaml";

// ── Fixed UUIDs from scripts/test/setup_test_db.sql ──────────────────────

pub const PRODUCT_1: &str = "00000000-0000-0000-0000-000000000001";
pub const PRODUCT_2: &str = "00000000-0000-0000-0000-000000000002";
pub const PRODUCT_3: &str = "00000000-0000-0000-0000-000000000003";
pub const PRODUCT_4: &str = "00000000-0000-0000-0000-000000000004"; // inactive
pub const PRODUCT_5: &str = "00000000-0000-0000-0000-000000000005";

pub const ORDER_1: &str = "10000000-0000-0000-0000-000000000001";
pub const ORDER_2: &str = "10000000-0000-0000-0000-000000000002";

// ── Postgres helpers ──────────────────────────────────────────────────────

pub async fn pg_pool() -> Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(5)
        .connect(PG_URL)
        .await?)
}

/// Re-seed the database to a clean, deterministic state.
pub async fn reseed(pool: &PgPool) -> Result<()> {
    sqlx::query("CALL seed_test_data()").execute(pool).await?;
    Ok(())
}

// ── Elasticsearch helpers ─────────────────────────────────────────────────

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

/// GET /<index>/_doc/<id> → `_source` object, or `None` if 404.
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

/// POST /<index>/_search match_all → array of hit objects.
pub async fn es_all(index: &str) -> Result<Vec<Value>> {
    let resp = http()
        .post(format!("{ES_URL}/{index}/_search"))
        .json(&serde_json::json!({ "query": { "match_all": {} }, "size": 1000 }))
        .send()
        .await?;
    let data: Value = resp.json().await?;
    Ok(data["hits"]["hits"].as_array().cloned().unwrap_or_default())
}

/// DELETE /<index> — silently ignores 404.
pub async fn es_delete_index(index: &str) -> Result<()> {
    let _ = http().delete(format!("{ES_URL}/{index}")).send().await;
    Ok(())
}

/// POST /<index>/_refresh — makes recently indexed docs immediately searchable.
pub async fn es_refresh(index: &str) -> Result<()> {
    http()
        .post(format!("{ES_URL}/{index}/_refresh"))
        .send()
        .await?;
    Ok(())
}

// ── Polling helper ────────────────────────────────────────────────────────

/// Poll `predicate` every `interval` until it returns `true` or `timeout` expires.
/// Returns whether the predicate returned `true` before the deadline.
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

// ── GraphQL helpers ───────────────────────────────────────────────────────

/// POST a GraphQL query to GQL_URL. Returns the full JSON response body.
pub async fn gql(query: &str, variables: Option<Value>) -> Result<Value> {
    let body = match variables {
        Some(v) => serde_json::json!({ "query": query, "variables": v }),
        None => serde_json::json!({ "query": query }),
    };
    let resp = http().post(GQL_URL).json(&body).send().await?;
    Ok(resp.json().await?)
}

/// Panic if the GraphQL response contains non-empty `errors`.
pub fn assert_no_gql_errors(resp: &Value) {
    if let Some(errs) = resp.get("errors") {
        if !errs.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            panic!("GraphQL errors:\n{errs:#}");
        }
    }
}
