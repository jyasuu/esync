/// tests/test_graphql.rs
/// Integration tests for `esync serve`.
/// Starts the Axum/GraphQL server on port 4001, fires real HTTP requests.
/// Requires: Postgres (esync_test db seeded). No Elasticsearch needed.

mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db};
use std::time::Duration;
use tokio::task::JoinHandle;

// ── Server lifecycle ──────────────────────────────────────────────────────

/// Spawn `esync serve` on 127.0.0.1:4001 as a background task.
/// Returns when the server is ready to accept connections (polls up to 5 s).
async fn start_server(cfg: Config) -> JoinHandle<()> {
    let args = esync::commands::serve::ServeArgs {
        host:          Some("127.0.0.1".into()),
        port:          Some(4001),
        no_playground: true,
    };
    let handle = tokio::spawn(async move {
        let _ = esync::commands::serve::run(cfg, args).await;
    });

    // Poll until the health endpoint responds
    let ready = wait_until(
        Duration::from_secs(10),
        Duration::from_millis(100),
        || async {
            reqwest::Client::new()
                .get("http://127.0.0.1:4001/healthz")
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        },
    ).await;

    assert!(ready, "GraphQL server did not become ready within 10 s");
    handle
}

async fn setup() -> Result<(JoinHandle<()>, sqlx::PgPool)> {
    let cfg  = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    reseed(&pool).await?;
    let server = start_server(cfg).await;
    Ok((server, pool))
}

// ── list_product ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_returns_all_products() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let resp = gql("{ list_product(limit: 20) { id name price stock active } }", None).await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["list_product"].as_array().unwrap();
    assert_eq!(items.len(), 5, "Expected 5 products");

    srv.abort(); let _ = srv.await;
    Ok(())
}

#[tokio::test]
async fn test_list_pagination_no_duplicates() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let p1 = gql("{ list_product(limit: 2, offset: 0) { id } }", None).await?;
    let p2 = gql("{ list_product(limit: 2, offset: 2) { id } }", None).await?;
    let p3 = gql("{ list_product(limit: 2, offset: 4) { id } }", None).await?;

    for r in [&p1, &p2, &p3] { assert_no_gql_errors(r); }

    let ids_on = |r: &serde_json::Value| -> Vec<String> {
        r["data"]["list_product"]
            .as_array().unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };

    let mut all: Vec<String> = [ids_on(&p1), ids_on(&p2), ids_on(&p3)].concat();
    assert_eq!(all.len(), 5);
    all.sort(); all.dedup();
    assert_eq!(all.len(), 5, "No duplicate IDs across pages");

    srv.abort(); let _ = srv.await;
    Ok(())
}

#[tokio::test]
async fn test_list_search_filters_by_text() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let resp = gql(r#"{ list_product(search: "Widget") { id name } }"#, None).await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["list_product"].as_array().unwrap();
    assert!(!items.is_empty(), "Expected at least 1 match for 'Widget'");
    for item in items {
        let name = item["name"].as_str().unwrap().to_lowercase();
        assert!(name.contains("widget"), "Unexpected name in results: {name}");
    }

    srv.abort(); let _ = srv.await;
    Ok(())
}

#[tokio::test]
async fn test_list_search_empty_result() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let resp = gql(r#"{ list_product(search: "xyzzy_no_match_zzzz") { id } }"#, None).await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["list_product"].as_array().unwrap();
    assert_eq!(items.len(), 0);

    srv.abort(); let _ = srv.await;
    Ok(())
}

// ── get_product ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_product_by_id_returns_correct_fields() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let q = format!(r#"{{ get_product(id: "{PRODUCT_1}") {{ id name price stock active }} }}"#);
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let p = &resp["data"]["get_product"];
    assert_eq!(p["id"],     PRODUCT_1,      "id mismatch");
    assert_eq!(p["name"],   "Alpha Widget",  "name mismatch");
    assert_eq!(p["active"], true,            "active mismatch");
    assert!(p["price"].as_f64().is_some(),   "price should be numeric");

    srv.abort(); let _ = srv.await;
    Ok(())
}

#[tokio::test]
async fn test_get_product_not_found_returns_null() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let resp = gql(
        r#"{ get_product(id: "00000000-0000-0000-0000-000000000099") { id } }"#,
        None,
    ).await?;
    assert_no_gql_errors(&resp);
    assert!(resp["data"]["get_product"].is_null());

    srv.abort(); let _ = srv.await;
    Ok(())
}

// ── list_order (SQL filter) ───────────────────────────────────────────────

#[tokio::test]
async fn test_list_order_excludes_soft_deleted() -> Result<()> {
    let (srv, pool) = setup().await?;

    sqlx::query("UPDATE orders SET deleted_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(ORDER_1)?)
        .execute(&pool).await?;

    let resp = gql("{ list_order(limit: 20) { id status } }", None).await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["list_order"].as_array().unwrap();
    assert_eq!(items.len(), 2, "Soft-deleted order must be excluded");
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert!(!ids.contains(&ORDER_1), "Soft-deleted ORDER_1 must not appear");

    reseed(&pool).await?;
    srv.abort(); let _ = srv.await;
    Ok(())
}

// ── Field types ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_numeric_fields_are_json_numbers() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let q = format!(r#"{{ get_product(id: "{PRODUCT_2}") {{ price stock }} }}"#);
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let p = &resp["data"]["get_product"];
    assert!(p["price"].is_number(), "price must be a JSON number, got: {}", p["price"]);
    assert!(p["stock"].is_number(), "stock must be a JSON number, got: {}", p["stock"]);

    srv.abort(); let _ = srv.await;
    Ok(())
}
