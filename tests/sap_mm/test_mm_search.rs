/// tests/sap_mm/test_mm_search.rs
/// Integration tests for ES-backed search_material_master and search_vendor_master.
///
/// Search GQL shape (from test_search.rs):
///   search_<entity>(q: "...", limit: N, offset: N, filter: "...", sort: "...")
///   → { total  took  items { <scalar fields>  _score  _highlight { field ... _all }  <relations> } }
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;
use std::time::Duration;
use tokio::task::JoinHandle;

// ── Lifecycle ─────────────────────────────────────────────────────────────────

async fn start_server(cfg: Config) -> JoinHandle<()> {
    let args = esync::commands::serve::ServeArgs {
        host: Some("127.0.0.1".into()),
        port: Some(4002),
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
                .get("http://127.0.0.1:4002/healthz")
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        },
    )
    .await;
    assert!(ready, "MM GraphQL server did not become ready within 10 s");
    handle
}

async fn setup() -> Result<(JoinHandle<()>, sqlx::PgPool, EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;
    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
        es_refresh(&entity.index).await?;
    }
    let server = start_server(cfg.clone()).await;
    Ok((server, pool, es, cfg))
}

// ── search_material_master ────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_search_material_by_description() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "hydraulic", limit: 5) {
            total took
            items { id material_number description }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let page = &resp["data"]["search_material_master"];
    assert!(
        page["total"].as_i64().unwrap_or(0) >= 1,
        "Should find materials matching 'hydraulic'"
    );

    let items = page["items"].as_array().unwrap();
    assert!(
        items.iter().any(|h| h["material_number"] == "MAT-2000"),
        "MAT-2000 (Hydraulic Pump) must be in results"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_by_material_number() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "MAT-1000", limit: 5) {
            total
            items { id material_number description }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    assert!(
        !items.is_empty(),
        "Should find MAT-1000 by its material number"
    );
    assert_eq!(items[0]["material_number"], "MAT-1000");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_by_group() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "MG-STEEL", limit: 10) {
            total
            items { id material_number material_group }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    assert!(
        !items.is_empty(),
        "MG-STEEL group search should return results"
    );
    // Top hit should be MG-STEEL; we don't assert all because ES may return
    // lower-scored hits from other groups via search_text partial matches.
    assert!(
        items.iter().any(|h| h["material_group"] == "MG-STEEL"),
        "At least one MG-STEEL material must appear in results"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_with_enriched_stock() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "steel sheet", limit: 5) {
            items {
                material_number
                stock_levels { plant sloc unrestricted_stock }
            }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    assert!(
        !items.is_empty(),
        "steel sheet search should return results"
    );

    let mat = items
        .iter()
        .find(|h| h["material_number"] == "MAT-1000")
        .unwrap();
    let stocks = mat["stock_levels"].as_array().unwrap();
    assert!(
        !stocks.is_empty(),
        "stock_levels should be enriched via enrich config"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_with_enriched_plant_views() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "bolt", limit: 5) {
            items {
                material_number
                plant_views { plant standard_price reorder_point }
            }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    let bolt = items.iter().find(|h| h["material_number"] == "MAT-1001");
    assert!(
        bolt.is_some(),
        "MAT-1001 (bolt) should appear in search results"
    );

    let views = bolt.unwrap()["plant_views"].as_array().unwrap();
    assert!(
        !views.is_empty(),
        "plant_views enrichment should work for bolt"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_no_results_for_garbage() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "xyzzy_nonexistent_zzz", limit: 5) {
            total items { id }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let total = resp["data"]["search_material_master"]["total"]
        .as_i64()
        .unwrap_or(-1);
    assert_eq!(total, 0, "Nonsense query should return zero results");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_limit_respected() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "ROH", limit: 1) {
            total items { id material_type }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 1, "limit: 1 must return exactly 1 result");

    let total = resp["data"]["search_material_master"]["total"]
        .as_i64()
        .unwrap_or(0);
    assert!(
        total >= 1,
        "total should reflect all matching docs, not just the page"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_highlight() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "Carbon", limit: 5) {
            items {
                material_number
                _highlight { description material_number _all }
            }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    assert!(!items.is_empty(), "Should find carbon steel");

    // At least one hit should carry a highlight with <em>
    let has_em = items.iter().any(|item| {
        ["description", "material_number", "_all"].iter().any(|f| {
            item["_highlight"][f]
                .as_str()
                .map(|s| s.contains("<em>"))
                .unwrap_or(false)
        })
    });
    assert!(
        has_em,
        "At least one result should have a highlighted field containing <em>"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_score_present() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_material_master(q: "pump", limit: 5) {
            items { id material_number _score }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_material_master"]["items"]
        .as_array()
        .unwrap();
    assert!(!items.is_empty());
    for item in items {
        assert!(
            item["_score"].as_f64().is_some() || item["_score"].as_i64().is_some(),
            "_score must be numeric on every hit"
        );
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── search_vendor_master ──────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_by_name() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_vendor_master(q: "Acme", limit: 5) {
            total took
            items { id vendor_number name country }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_vendor_master"]["items"]
        .as_array()
        .unwrap();
    assert!(!items.is_empty(), "Should find Acme by name");
    assert_eq!(items[0]["vendor_number"], "V000001");
    assert_eq!(items[0]["country"], "US");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_by_vendor_number() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_vendor_master(q: "V000002", limit: 5) {
            items { vendor_number name }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_vendor_master"]["items"]
        .as_array()
        .unwrap();
    assert!(
        !items.is_empty(),
        "Should find GlobalParts by vendor number"
    );
    assert_eq!(items[0]["vendor_number"], "V000002");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_by_city() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_vendor_master(q: "Stuttgart", limit: 5) {
            items { vendor_number name city }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_vendor_master"]["items"]
        .as_array()
        .unwrap();
    assert!(
        !items.is_empty(),
        "Should find GlobalParts by city Stuttgart"
    );
    assert!(
        items.iter().any(|h| h["vendor_number"] == "V000002"),
        "GlobalParts must appear"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_highlight() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_vendor_master(q: "Pacific", limit: 5) {
            items {
                vendor_number name
                _highlight { name vendor_number _all }
            }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["search_vendor_master"]["items"]
        .as_array()
        .unwrap();
    assert!(!items.is_empty(), "Should find Pacific Plastics");

    // Verify highlight structure is present (may or may not contain <em>
    // depending on whether the field is text-analyzed)
    for item in items {
        let hl = &item["_highlight"];
        assert!(
            !hl.is_null(),
            "_highlight object should be present on each hit"
        );
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_no_results_for_garbage() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_vendor_master(q: "xyzzy_no_vendor_zzzz", limit: 5) {
            total items { id }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let total = resp["data"]["search_vendor_master"]["total"]
        .as_i64()
        .unwrap_or(-1);
    assert_eq!(total, 0, "Nonsense query should return zero results");

    srv.abort();
    let _ = srv.await;
    Ok(())
}
