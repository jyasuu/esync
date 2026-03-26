/// tests/sap_mm/test_mm_graphql.rs
/// Integration tests for `esync serve` over the SAP MM schema.
/// Covers list/get queries, nested relations, and pagination.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, indexer};
use serial_test::serial;
use std::time::Duration;
use tokio::task::JoinHandle;

// ── Server lifecycle ──────────────────────────────────────────────────────────

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

async fn setup() -> Result<(JoinHandle<()>, sqlx::PgPool, esync::elastic::EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = esync::elastic::EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;
    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
        es_refresh(&entity.index).await?;
    }
    let server = start_server(cfg.clone()).await;
    Ok((server, pool, es, cfg))
}

// ── list_MaterialMaster ───────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_list_material_count() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql("{ list_MaterialMaster(limit: 20) { id material_number } }", None).await?;
    assert_no_gql_errors(&resp);

    let items = resp["data"]["list_MaterialMaster"].as_array().unwrap();
    // filter: deleted_at IS NULL → 4 active materials
    assert_eq!(items.len(), 4, "Should return 4 active materials");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_gql_list_material_pagination() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let p1 = gql("{ list_MaterialMaster(limit: 2, offset: 0) { id } }", None).await?;
    let p2 = gql("{ list_MaterialMaster(limit: 2, offset: 2) { id } }", None).await?;
    assert_no_gql_errors(&p1);
    assert_no_gql_errors(&p2);

    let ids1: Vec<&str> = p1["data"]["list_MaterialMaster"].as_array().unwrap()
        .iter().filter_map(|v| v["id"].as_str()).collect();
    let ids2: Vec<&str> = p2["data"]["list_MaterialMaster"].as_array().unwrap()
        .iter().filter_map(|v| v["id"].as_str()).collect();

    assert_eq!(ids1.len(), 2);
    assert_eq!(ids2.len(), 2);
    // No overlapping ids across pages
    for id in &ids1 {
        assert!(!ids2.contains(id), "Pagination must not return duplicates");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── get_MaterialMaster ────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_get_material_fields() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        &format!(
            r#"{{ get_MaterialMaster(id: "{MAT_STEEL}") {{
                material_number description material_type material_group base_unit gross_weight
            }} }}"#
        ),
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let mat = &resp["data"]["get_MaterialMaster"];
    assert_eq!(mat["material_number"], "MAT-1000");
    assert_eq!(mat["material_type"], "ROH");
    assert_eq!(mat["material_group"], "MG-STEEL");
    assert_eq!(mat["base_unit"], "EA");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_gql_get_material_not_found() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        "{ get_MaterialMaster(id: \"00000000-dead-beef-0000-000000000000\") { id } }",
        None,
    ).await?;
    assert_no_gql_errors(&resp);
    assert!(resp["data"]["get_MaterialMaster"].is_null(), "Unknown ID must return null");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Nested relations — plant_views ────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_material_with_plant_views() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        &format!(
            r#"{{ get_MaterialMaster(id: "{MAT_STEEL}") {{
                material_number
                plant_views {{
                    plant mrp_type standard_price reorder_point purchasing_group
                }}
            }} }}"#
        ),
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let views = resp["data"]["get_MaterialMaster"]["plant_views"].as_array().unwrap();
    assert_eq!(views.len(), 1, "MAT-1000 has one plant_data row");
    assert_eq!(views[0]["plant"], "1000");
    assert_eq!(views[0]["mrp_type"], "PD");
    assert_eq!(views[0]["purchasing_group"], "EK1");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Nested relations — stock_levels ──────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_material_with_stock_levels() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        &format!(
            r#"{{ get_MaterialMaster(id: "{MAT_STEEL}") {{
                material_number
                stock_levels {{
                    plant sloc unrestricted_stock quality_stock blocked_stock
                }}
            }} }}"#
        ),
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let stocks = resp["data"]["get_MaterialMaster"]["stock_levels"].as_array().unwrap();
    assert_eq!(stocks.len(), 1, "MAT-1000 has one storage_location row");
    assert_eq!(stocks[0]["plant"], "1000");
    assert_eq!(stocks[0]["sloc"], "0001");

    let qty = parse_numeric(&stocks[0]["unrestricted_stock"]).unwrap_or(0.0);
    assert!((qty - 150.0).abs() < 0.01, "unrestricted_stock should be 150, got {qty}");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Nested relations — purchasing_infos with nested vendor ────────────────────

#[tokio::test]
#[serial]
async fn test_gql_material_purchasing_infos_with_vendor() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        &format!(
            r#"{{ get_MaterialMaster(id: "{MAT_STEEL}") {{
                material_number
                purchasing_infos {{
                    purchasing_org net_price currency planned_delivery
                    vendor {{ vendor_number name country }}
                }}
            }} }}"#
        ),
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let infos = resp["data"]["get_MaterialMaster"]["purchasing_infos"].as_array().unwrap();
    assert_eq!(infos.len(), 2, "MAT-1000 has 2 purchasing info records");

    // Both vendors should be resolved
    for info in infos {
        assert!(!info["vendor"]["vendor_number"].is_null(), "vendor must be resolved");
    }

    // Verify Acme's price
    let acme = infos.iter().find(|i| i["vendor"]["vendor_number"] == "V000001").unwrap();
    let price = parse_numeric(&acme["net_price"]).unwrap_or(0.0);
    assert!((price - 78.5).abs() < 0.01, "Acme net_price should be 78.50, got {price}");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Nested relations — movements ──────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_material_movements() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        &format!(
            r#"{{ get_MaterialMaster(id: "{MAT_STEEL}") {{
                material_number
                movements {{
                    doc_number movement_type quantity unit posting_date
                    vendor {{ vendor_number name }}
                }}
            }} }}"#
        ),
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let moves = resp["data"]["get_MaterialMaster"]["movements"].as_array().unwrap();
    assert!(!moves.is_empty(), "MAT-1000 should have goods movement records");

    let gr = moves.iter().find(|m| m["movement_type"] == "101").unwrap();
    assert_eq!(gr["doc_number"], "5000000001");
    assert!(!gr["vendor"]["vendor_number"].is_null(), "GR vendor must be resolved");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── list_VendorMaster ─────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_list_vendor_count() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql("{ list_VendorMaster(limit: 20) { id vendor_number name country } }", None).await?;
    assert_no_gql_errors(&resp);

    let vendors = resp["data"]["list_VendorMaster"].as_array().unwrap();
    assert_eq!(vendors.len(), 3, "Should return all 3 vendors");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Vendor → purchasing_infos → material ──────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_vendor_purchasing_infos_with_material() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        &format!(
            r#"{{ get_VendorMaster(id: "{VENDOR_ACME}") {{
                vendor_number name
                purchasing_infos {{
                    purchasing_org net_price currency
                    material {{ material_number description }}
                }}
            }} }}"#
        ),
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let infos = resp["data"]["get_VendorMaster"]["purchasing_infos"].as_array().unwrap();
    assert_eq!(infos.len(), 2, "Acme supplies 2 materials");

    for info in infos {
        assert!(!info["material"]["material_number"].is_null(), "material must be resolved");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── list_StorageLocation ──────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_list_storage_location_with_filter() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        "{ list_StorageLocation(filter: \"plant = '1000'\", limit: 20) { plant sloc unrestricted_stock } }",
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let rows = resp["data"]["list_StorageLocation"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "All 4 storage_location rows are in plant 1000");
    for row in rows {
        assert_eq!(row["plant"], "1000");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── list_MaterialDocument — filter by movement_type ───────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_list_movements_by_type() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        "{ list_MaterialDocument(filter: \"movement_type = '101'\", limit: 20) { doc_number movement_type quantity } }",
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let docs = resp["data"]["list_MaterialDocument"].as_array().unwrap();
    assert!(!docs.is_empty(), "Should have GR (101) documents");
    for doc in docs {
        assert_eq!(doc["movement_type"], "101");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── list_PurchasingInfo — filter by purchasing_org ────────────────────────────

#[tokio::test]
#[serial]
async fn test_gql_list_purchasing_info_by_org() -> Result<()> {
    let (srv, _pool, _es, _cfg) = setup().await?;

    let resp = gql(
        "{ list_PurchasingInfo(filter: \"purchasing_org = 'EU01'\", limit: 20) { purchasing_org net_price currency } }",
        None,
    ).await?;
    assert_no_gql_errors(&resp);

    let infos = resp["data"]["list_PurchasingInfo"].as_array().unwrap();
    assert_eq!(infos.len(), 3, "All 3 purchasing info rows belong to EU01");
    for info in infos {
        assert_eq!(info["purchasing_org"], "EU01");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}
