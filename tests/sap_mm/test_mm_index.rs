/// tests/sap_mm/test_mm_index.rs
/// Integration tests for `esync index` against the SAP MM schema.
/// Verifies that bulk indexing populates ES correctly for all six entities.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;

// ── Shared setup ──────────────────────────────────────────────────────────────

async fn setup() -> Result<(sqlx::PgPool, EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;

    reseed(&pool).await?;

    // Wipe all test indices before each test group
    for idx in [IDX_MATERIAL, IDX_PLANT, IDX_STOCK, IDX_VENDOR, IDX_PURCH_INFO, IDX_MATERIAL_DOC] {
        es_delete_index(idx).await?;
    }

    Ok((pool, es, cfg))
}

// ── Material Master ───────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_index_material_count() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("MaterialMaster").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_MATERIAL).await?;

    // filter: deleted_at IS NULL → MAT-INACT should be excluded
    let count = es_count(IDX_MATERIAL).await?;
    assert_eq!(count, 4, "Only 4 non-deleted materials expected in ES");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_material_fields() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("MaterialMaster").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_MATERIAL).await?;

    let doc = es_get(IDX_MATERIAL, MAT_STEEL).await?.expect("MAT-1000 must be indexed");
    assert_eq!(doc["material_number"], "MAT-1000");
    assert_eq!(doc["material_type"], "ROH");
    assert_eq!(doc["material_group"], "MG-STEEL");
    assert_eq!(doc["base_unit"], "EA");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_material_search_text_built() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("MaterialMaster").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_MATERIAL).await?;

    let doc = es_get(IDX_MATERIAL, MAT_PUMP).await?.expect("MAT-2000 must be indexed");
    let search_text = doc["search_text"].as_str().unwrap_or("");
    // search_text concatenates description + material_number + group + type
    assert!(
        search_text.contains("Hydraulic") && search_text.contains("MAT-2000"),
        "search_text should include description and material number, got: {search_text}"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_deleted_material_excluded() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("MaterialMaster").unwrap();

    // Soft-delete MAT-INACT
    sqlx::query("UPDATE material_master SET deleted_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(MAT_INACT)?)
        .execute(&pool)
        .await?;

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_MATERIAL).await?;

    let doc = es_get(IDX_MATERIAL, MAT_INACT).await?;
    assert!(doc.is_none(), "Soft-deleted material must not appear in ES");

    Ok(())
}

// ── Plant Data ────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_index_plant_data_count() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("PlantData").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_PLANT).await?;

    let count = es_count(IDX_PLANT).await?;
    assert_eq!(count, 4, "All 4 plant_data rows should be indexed");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_plant_data_price() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("PlantData").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_PLANT).await?;

    // Find the plant record for MAT_STEEL
    let hits = es_search(IDX_PLANT, "EK1").await?;
    let steel_record = hits.iter().find(|h| {
        h["_source"]["material_id"].as_str() == Some(MAT_STEEL)
    });
    assert!(steel_record.is_some(), "plant_data for MAT-1000 (EK1 group) must be indexed");

    let src = &steel_record.unwrap()["_source"];
    let price = parse_numeric(&src["standard_price"]).unwrap_or(0.0);
    assert!((price - 85.0).abs() < 0.01, "standard_price should be 85.00, got {price}");

    Ok(())
}

// ── Storage Location ──────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_index_stock_count() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("StorageLocation").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_STOCK).await?;

    let count = es_count(IDX_STOCK).await?;
    assert_eq!(count, 4, "All 4 storage_location rows should be indexed");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_stock_values() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("StorageLocation").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_STOCK).await?;

    let hits = es_all(IDX_STOCK).await?;
    let steel_stock = hits.iter().find(|h| {
        h["_source"]["material_id"].as_str() == Some(MAT_STEEL)
    });
    assert!(steel_stock.is_some(), "Stock for MAT-1000 must exist");

    let src = &steel_stock.unwrap()["_source"];
    let qty = parse_numeric(&src["unrestricted_stock"]).unwrap_or(0.0);
    assert!((qty - 150.0).abs() < 0.01, "unrestricted_stock should be 150, got {qty}");

    Ok(())
}

// ── Vendor Master ─────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_index_vendor_count() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("VendorMaster").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_VENDOR).await?;

    let count = es_count(IDX_VENDOR).await?;
    assert_eq!(count, 3, "All 3 active vendors should be indexed");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_vendor_fields() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("VendorMaster").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_VENDOR).await?;

    let doc = es_get(IDX_VENDOR, VENDOR_ACME).await?.expect("Acme must be indexed");
    assert_eq!(doc["vendor_number"], "V000001");
    assert_eq!(doc["name"], "Acme Steel Corp");
    assert_eq!(doc["country"], "US");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_vendor_search_text_built() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("VendorMaster").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_VENDOR).await?;

    let doc = es_get(IDX_VENDOR, VENDOR_GLOBAL).await?.expect("GlobalParts must be indexed");
    let st = doc["search_text"].as_str().unwrap_or("");
    assert!(
        st.contains("GlobalParts") && st.contains("V000002"),
        "search_text should include name and vendor number, got: {st}"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_soft_deleted_vendor_excluded() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("VendorMaster").unwrap();

    sqlx::query("UPDATE vendor_master SET deleted_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(VENDOR_PACIFIC)?)
        .execute(&pool)
        .await?;

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_VENDOR).await?;

    let doc = es_get(IDX_VENDOR, VENDOR_PACIFIC).await?;
    assert!(doc.is_none(), "Soft-deleted vendor must not appear in ES");

    let count = es_count(IDX_VENDOR).await?;
    assert_eq!(count, 2, "Only 2 active vendors after soft-delete");

    Ok(())
}

// ── Purchasing Info ───────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_index_purchasing_info_count() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("PurchasingInfo").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_PURCH_INFO).await?;

    let count = es_count(IDX_PURCH_INFO).await?;
    assert_eq!(count, 3, "All 3 purchasing info records should be indexed");

    Ok(())
}

// ── Material Document ─────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_index_material_doc_count() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("MaterialDocument").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_MATERIAL_DOC).await?;

    let count = es_count(IDX_MATERIAL_DOC).await?;
    assert_eq!(count, 3, "All 3 goods movement docs should be indexed");

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_index_material_doc_movement_types() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entity("MaterialDocument").unwrap();

    indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
    es_refresh(IDX_MATERIAL_DOC).await?;

    let hits = es_all(IDX_MATERIAL_DOC).await?;
    let types: Vec<&str> = hits.iter()
        .filter_map(|h| h["_source"]["movement_type"].as_str())
        .collect();

    assert!(types.contains(&"101"), "Goods receipt (101) must be indexed");
    assert!(types.contains(&"261"), "Goods issue (261) must be indexed");

    Ok(())
}

// ── Full rebuild — all entities ───────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_full_rebuild_all_entities() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
        es_refresh(&entity.index).await?;
    }

    assert_eq!(es_count(IDX_MATERIAL).await?,     4, "materials");
    assert_eq!(es_count(IDX_PLANT).await?,         4, "plant_data rows");
    assert_eq!(es_count(IDX_STOCK).await?,         4, "stock rows");
    assert_eq!(es_count(IDX_VENDOR).await?,        3, "vendors");
    assert_eq!(es_count(IDX_PURCH_INFO).await?,    3, "purchasing_info rows");
    assert_eq!(es_count(IDX_MATERIAL_DOC).await?,  3, "material_documents");

    Ok(())
}
