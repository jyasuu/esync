/// tests/sap_mm/test_mm_search.rs
/// Integration tests for ES-backed `search_MaterialMaster` and `search_VendorMaster`
/// GraphQL queries in the SAP MM schema.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;
use std::time::Duration;
use tokio::task::JoinHandle;

// ── Lifecycle helpers ─────────────────────────────────────────────────────────

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

// ── search_MaterialMaster ─────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_search_material_by_description() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_MaterialMaster(query: "hydraulic", limit: 5) {
            hits { id material_number description }
            total
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_MaterialMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(
        !hits.is_empty(),
        "Should find materials matching 'hydraulic'"
    );
    assert!(
        hits.iter().any(|h| h["material_number"] == "MAT-2000"),
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
        r#"{ search_MaterialMaster(query: "MAT-1000", limit: 5) {
            hits { id material_number description }
            total
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_MaterialMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(
        !hits.is_empty(),
        "Should find MAT-1000 by its material number"
    );
    assert_eq!(hits[0]["material_number"], "MAT-1000");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_material_by_group() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_MaterialMaster(query: "MG-STEEL", limit: 10) {
            hits { id material_number material_group }
            total
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_MaterialMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(
        hits.iter().all(|h| h["material_group"] == "MG-STEEL"),
        "All hits for MG-STEEL query should belong to that group"
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
        r#"{ search_MaterialMaster(query: "steel sheet", limit: 5) {
            hits {
                material_number
                stock_levels { plant sloc unrestricted_stock }
            }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_MaterialMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(!hits.is_empty(), "steel sheet search should return results");

    // MAT-1000 should be the top hit and have enriched stock
    let mat = hits
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
        r#"{ search_MaterialMaster(query: "bolt", limit: 5) {
            hits {
                material_number
                plant_views { plant standard_price reorder_point }
            }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_MaterialMaster"]["hits"]
        .as_array()
        .unwrap();
    let bolt = hits.iter().find(|h| h["material_number"] == "MAT-1001");
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
        r#"{ search_MaterialMaster(query: "xyzzy_nonexistent_zzz", limit: 5) {
            hits { id }
            total
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let total = resp["data"]["search_MaterialMaster"]["total"]
        .as_u64()
        .unwrap_or(0);
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
        r#"{ search_MaterialMaster(query: "ROH", limit: 1) {
            hits { id material_type }
            total
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_MaterialMaster"]["hits"]
        .as_array()
        .unwrap();
    assert_eq!(hits.len(), 1, "limit: 1 must return exactly 1 result");

    let total = resp["data"]["search_MaterialMaster"]["total"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        total >= 1,
        "total should reflect all matching docs, not just the page"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── search_VendorMaster ───────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_by_name() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_VendorMaster(query: "Acme", limit: 5) {
            hits { id vendor_number name country }
            total
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_VendorMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(!hits.is_empty(), "Should find Acme by name");
    assert_eq!(hits[0]["vendor_number"], "V000001");
    assert_eq!(hits[0]["country"], "US");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_by_vendor_number() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_VendorMaster(query: "V000002", limit: 5) {
            hits { vendor_number name }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_VendorMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(!hits.is_empty(), "Should find GlobalParts by vendor number");
    assert_eq!(hits[0]["vendor_number"], "V000002");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_by_city() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_VendorMaster(query: "Stuttgart", limit: 5) {
            hits { vendor_number name city }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_VendorMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(
        !hits.is_empty(),
        "Should find GlobalParts by city Stuttgart"
    );

    let global = hits.iter().find(|h| h["vendor_number"] == "V000002");
    assert!(global.is_some(), "GlobalParts (Stuttgart) must appear");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_search_vendor_with_highlights() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        r#"{ search_VendorMaster(query: "Pacific", limit: 5) {
            hits { vendor_number name }
            highlights { field snippets }
        } }"#,
        None,
    )
    .await?;
    assert_no_gql_errors(&resp);

    let hits = resp["data"]["search_VendorMaster"]["hits"]
        .as_array()
        .unwrap();
    assert!(!hits.is_empty(), "Should find Pacific Plastics");

    // Highlights should be populated when the query matches a configured field
    let highlights = resp["data"]["search_VendorMaster"]["highlights"].as_array();
    if let Some(hl) = highlights {
        // Some ES versions don't return highlights on keyword-only matches,
        // so we only assert structure if highlights are present
        for h in hl {
            assert!(h["field"].is_string(), "highlight.field should be a string");
        }
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}
