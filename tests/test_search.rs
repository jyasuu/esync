/// tests/test_search.rs
/// Integration tests for the ES-backed search_* GraphQL queries.
///
/// Tests cover:
///   - basic full-text search (multi_match)
///   - score + highlight in response
///   - ES filter argument (JSON DSL)
///   - live_columns override (fresh PG values on ES hits)
///   - enrich: belongs_to, has_many, many_to_many on search results
///   - cross_index search
///   - empty result set
///   - pagination (limit / offset)
///
/// Requires: Postgres (esync_test) + Elasticsearch + seeded + indexed data.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;
use std::{sync::Arc, time::Duration};
use tokio::task::JoinHandle;

// ── Setup ─────────────────────────────────────────────────────────────────

async fn setup() -> Result<(JoinHandle<()>, sqlx::PgPool, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;

    reseed(&pool).await?;

    // Index all entities that have an ES index defined
    for entity in &cfg.entities {
        es_delete_index(&entity.index).await?;
        indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
        es_refresh(&entity.index).await?;
    }

    let srv = start_server(cfg.clone()).await;
    Ok((srv, pool, cfg))
}

async fn start_server(cfg: Config) -> JoinHandle<()> {
    let args = esync::commands::serve::ServeArgs {
        host: Some("127.0.0.1".into()),
        port: Some(4001),
        no_playground: true,
    };
    let handle = tokio::spawn(async move {
        let _ = esync::commands::serve::run(cfg, args).await;
    });
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
    )
    .await;
    assert!(ready, "Server did not become ready within 10 s");
    handle
}

// ── Basic full-text search ────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_returns_matching_products() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_product(q: "Widget") {
        total took
        items { _score id name price stock }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let page = &resp["data"]["search_product"];
    assert!(
        page["total"].as_i64().unwrap_or(0) >= 1,
        "Should find at least 1 product"
    );

    let items = page["items"].as_array().unwrap();
    assert!(!items.is_empty());
    // All items should have a relevance score
    for item in items {
        assert!(
            item["_score"].as_f64().is_some() || item["_score"].as_i64().is_some(),
            "_score must be numeric"
        );
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_empty_q_returns_all() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    // No q = match_all
    let resp = gql(r#"{ search_product { total items { id name } } }"#, None).await?;
    assert_no_gql_errors(&resp);
    let total = resp["data"]["search_product"]["total"]
        .as_i64()
        .unwrap_or(0);
    assert_eq!(total, 5, "match_all should return all 5 products");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_no_results_for_nonsense() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_product(q: "xyzzy_no_match_9999") {
        total items { id }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    assert_eq!(
        resp["data"]["search_product"]["total"]
            .as_i64()
            .unwrap_or(-1),
        0
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Highlight ─────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_highlight_contains_em_tags() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_product(q: "Widget") {
        items { _highlight { name description _all } }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let items = resp["data"]["search_product"]["items"].as_array().unwrap();
    // At least one hit should have a highlight with <em> tags
    let has_highlight = items.iter().any(|item| {
        item["_highlight"]["name"]
            .as_str()
            .map(|s| s.contains("<em>"))
            .unwrap_or(false)
    });
    assert!(
        has_highlight,
        "At least one result should have highlighted name containing <em>"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── ES filter argument ────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_filter_term_active() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    // Filter to active=false products only (PRODUCT_4 = Delta Thing)
    let resp = gql(
        r#"{ search_product(filter: "{\"term\":{\"active\":false}}") {
        total items { id name }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let total = resp["data"]["search_product"]["total"]
        .as_i64()
        .unwrap_or(0);
    assert_eq!(total, 1, "Only Delta Thing is inactive");
    let name = resp["data"]["search_product"]["items"][0]["name"]
        .as_str()
        .unwrap_or("");
    assert_eq!(name, "Delta Thing");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_filter_range_price() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    // Products with price >= 40
    let resp = gql(
        r#"{ search_product(filter: "{\"range\":{\"price\":{\"gte\":40}}}") {
        total items { id name }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let total = resp["data"]["search_product"]["total"]
        .as_i64()
        .unwrap_or(0);
    // Beta Gizmo ($49.99) and Gamma Doohickey ($199.00)
    assert!(
        total >= 2,
        "Expected at least 2 products priced >= 40, got {total}"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── live_columns ──────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_live_columns_reflect_pg_state() -> Result<()> {
    let (srv, pool, _cfg) = setup().await?;

    // Update stock in PG — ES index is stale
    sqlx::query("UPDATE products SET stock = 9999 WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_1)?)
        .execute(&pool)
        .await?;
    // Do NOT re-index — ES still has old value

    let resp = gql(
        &format!(
            r#"{{ search_product(q: "Alpha") {{
        items {{ id name stock }}
    }}}}"#
        ),
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let items = resp["data"]["search_product"]["items"].as_array().unwrap();
    let alpha = items.iter().find(|i| i["id"].as_str() == Some(PRODUCT_1));
    assert!(alpha.is_some(), "Alpha Widget should appear in search");

    // stock should be 9999 (live from PG) not 100 (stale ES value)
    let stock = alpha.unwrap()["stock"]
        .as_i64()
        .or_else(|| {
            alpha.unwrap()["stock"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(-1);
    assert_eq!(stock, 9999, "live_columns must override stale ES value");

    reseed(&pool).await?;
    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── enrich (relations on search results) ─────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_enrich_has_many() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_customer(q: "Alice") {
        total items {
            id name
            orders { id status }
        }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let items = resp["data"]["search_customer"]["items"].as_array().unwrap();
    let alice = items.iter().find(|i| i["name"].as_str() == Some("Alice"));
    assert!(alice.is_some(), "Alice should appear in customer search");

    let orders = alice.unwrap()["orders"].as_array().unwrap();
    assert!(
        !orders.is_empty(),
        "Alice should have orders enriched on search result"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_enrich_many_to_many() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_customer(q: "Alice") {
        items {
            id name
            tags { id label }
        }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let items = resp["data"]["search_customer"]["items"].as_array().unwrap();
    let alice = items.iter().find(|i| i["name"].as_str() == Some("Alice"));
    assert!(alice.is_some());

    let tags = alice.unwrap()["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 2, "Alice should have 2 tags enriched");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Pagination ────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_pagination() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    let p1 = gql(
        r#"{ search_product(limit: 2, offset: 0) { total items { id } } }"#,
        None,
    )
    .await?;
    let p2 = gql(
        r#"{ search_product(limit: 2, offset: 2) { total items { id } } }"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&p1);
    assert_no_gql_errors(&p2);

    let p1_ids: Vec<&str> = p1["data"]["search_product"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    let p2_ids: Vec<&str> = p2["data"]["search_product"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();

    assert_eq!(p1_ids.len(), 2, "Page 1 should have 2 items");
    assert_eq!(p2_ids.len(), 2, "Page 2 should have 2 items");

    // No overlap between pages
    for id in &p1_ids {
        assert!(!p2_ids.contains(id), "Pages must not overlap");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Cross-index search ────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_cross_index() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    // Customer search is configured with cross_index: [Product]
    // Searching for "Widget" should find hits from BOTH indices
    let resp = gql(
        r#"{ search_customer(q: "Widget") {
        total items { _id name }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    // With cross-index enabled, product hits also appear
    let total = resp["data"]["search_customer"]["total"]
        .as_i64()
        .unwrap_or(0);
    assert!(
        total >= 1,
        "Cross-index search should find at least 1 result"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Sort ──────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_sort_by_field() -> Result<()> {
    let (srv, _pool, _cfg) = setup().await?;

    // Sort by price ascending
    let resp = gql(
        r#"{ search_product(sort: "[{\"price\":{\"order\":\"asc\"}}]") {
        items { id name }
    }}"#,
        None,
    )
    .await?;

    assert_no_gql_errors(&resp);
    let items = resp["data"]["search_product"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "Sort query should return results");

    srv.abort();
    let _ = srv.await;
    Ok(())
}
