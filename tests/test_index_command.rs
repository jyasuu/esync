/// tests/test_index_command.rs
/// End-to-end tests for `esync index` — full Postgres → ES rebuild.
/// Calls indexer::rebuild_index() directly (no subprocess).
/// Requires: Postgres (esync_test seeded) + Elasticsearch.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};

async fn setup() -> Result<(sqlx::PgPool, EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;
    Ok((pool, es, cfg))
}

// ── Basic rebuild ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_rebuild_indexes_all_rows() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entities.iter().find(|e| e.name == "Product").unwrap();

    es_delete_index(&entity.index).await?;
    indexer::rebuild_index(&pool, &es, entity).await?;
    es_refresh(&entity.index).await?;

    let hits = es_all(&entity.index).await?;
    assert_eq!(hits.len(), 5, "Expected 5 products indexed");
    Ok(())
}

#[tokio::test]
async fn test_rebuild_document_fields_match_db() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entities.iter().find(|e| e.name == "Product").unwrap();

    indexer::rebuild_index(&pool, &es, entity).await?;

    let doc = es.get_document(&entity.index, PRODUCT_1).await?;
    let src = &doc["_source"];
    assert_eq!(src["name"], "Alpha Widget");
    assert_eq!(src["stock"], 100);
    assert_eq!(src["active"], true);
    // price stored as NUMERIC may come back as string from PG; check existence
    assert!(!src["price"].is_null(), "price should not be null");
    Ok(())
}

#[tokio::test]
async fn test_rebuild_replaces_stale_data() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entities.iter().find(|e| e.name == "Product").unwrap();

    indexer::rebuild_index(&pool, &es, entity).await?;

    // Mutate in DB
    sqlx::query("UPDATE products SET name = 'Modified Name' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_1)?)
        .execute(&pool)
        .await?;

    // Second rebuild picks up the mutation
    indexer::rebuild_index(&pool, &es, entity).await?;
    es_refresh(&entity.index).await?;

    let doc = es.get_document(&entity.index, PRODUCT_1).await?;
    assert_eq!(doc["_source"]["name"], "Modified Name");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
async fn test_rebuild_respects_sql_filter() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    // Orders entity has filter: "deleted_at IS NULL"
    let entity = cfg.entities.iter().find(|e| e.name == "Order").unwrap();

    // Soft-delete ORDER_1
    sqlx::query("UPDATE orders SET deleted_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(ORDER_1)?)
        .execute(&pool)
        .await?;

    indexer::rebuild_index(&pool, &es, entity).await?;
    es_refresh(&entity.index).await?;

    let hits = es_all(&entity.index).await?;
    assert_eq!(hits.len(), 2, "Soft-deleted order must be excluded");

    let ids: Vec<&str> = hits.iter().filter_map(|h| h["_id"].as_str()).collect();
    assert!(!ids.contains(&ORDER_1));

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
async fn test_rebuild_multi_batch_indexes_all() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let entity = cfg.entities.iter().find(|e| e.name == "Product").unwrap();

    // Insert 15 extra products → 20 total, batch_size=10 forces 2 full batches
    for i in 6..=20i32 {
        sqlx::query("INSERT INTO products (name, price) VALUES ($1, $2)")
            .bind(format!("Batch Product {i}"))
            .bind(f64::from(i) * 1.5)
            .execute(&pool)
            .await?;
    }

    indexer::rebuild_index(&pool, &es, entity).await?;
    es_refresh(&entity.index).await?;

    let hits = es_all(&entity.index).await?;
    assert_eq!(
        hits.len(),
        20,
        "All 20 products across 2 batches should be indexed"
    );

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
async fn test_rebuild_all_entities_independently() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity).await?;
        es_refresh(&entity.index).await?;
    }

    assert_eq!(es_all("test_products").await?.len(), 5);
    assert_eq!(es_all("test_orders").await?.len(), 3);
    Ok(())
}
