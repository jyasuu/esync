/// tests/test_watch_cdc.rs
/// Integration tests for `esync watch` — Postgres LISTEN/NOTIFY → Elasticsearch CDC.
/// Spawns the watch loop as a background Tokio task, performs DB mutations,
/// then polls ES until the expected state appears or a timeout fires.
/// Requires: Postgres (esync_test seeded) + Elasticsearch.

mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use std::time::Duration;
use tokio::task::JoinHandle;

// ── Shared setup ──────────────────────────────────────────────────────────

async fn setup() -> Result<(sqlx::PgPool, EsClient, Config)> {
    let cfg  = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es   = EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;

    // Pre-build indices so CDC upserts land on existing indices
    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity).await?;
        es_refresh(&entity.index).await?;
    }

    Ok((pool, es, cfg))
}

/// Spawn the watch loop as a background task.
fn spawn_watch(cfg: Config) -> JoinHandle<()> {
    tokio::spawn(async move {
        let args = esync::commands::watch::WatchArgs { entity: vec![] };
        // run() loops forever; we abort() the handle to stop it
        let _ = esync::commands::watch::run(cfg, args).await;
    })
}

// ── INSERT ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cdc_insert_propagates_to_es() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;
    let watch = spawn_watch(cfg);

    let new_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO products (name, price, stock) VALUES ('CDC Widget', 7.77, 10) RETURNING id",
    )
    .fetch_one(&pool).await?;
    let id_str = new_id.to_string();

    let found = wait_until(
        Duration::from_secs(6),
        Duration::from_millis(200),
        || {
            let id = id_str.clone();
            async move {
                es_get("test_products", &id).await
                    .ok().flatten().is_some()
            }
        },
    ).await;

    watch.abort(); let _ = watch.await;
    assert!(found, "Inserted product should appear in ES within 6 s");

    let doc = es_get("test_products", &id_str).await?.unwrap();
    assert_eq!(doc["name"], "CDC Widget");

    reseed(&pool).await?;
    Ok(())
}

// ── UPDATE ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cdc_update_propagates_to_es() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;
    let watch = spawn_watch(cfg);

    sqlx::query("UPDATE products SET price = 99.99, updated_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_1)?)
        .execute(&pool).await?;

    let ok = wait_until(
        Duration::from_secs(6),
        Duration::from_millis(200),
        || async {
            es_get("test_products", PRODUCT_1).await
                .ok().flatten()
                .map(|d| (d["price"].as_f64().unwrap_or(0.0) - 99.99).abs() < 0.001)
                .unwrap_or(false)
        },
    ).await;

    watch.abort(); let _ = watch.await;
    assert!(ok, "Updated price should reflect in ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}

// ── DELETE ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cdc_delete_removes_from_es() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;

    // Confirm present before watch starts
    let before = es_get("test_products", PRODUCT_5).await?;
    assert!(before.is_some(), "PRODUCT_5 must exist in ES before the test");

    let watch = spawn_watch(cfg);

    sqlx::query("DELETE FROM products WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_5)?)
        .execute(&pool).await?;

    let removed = wait_until(
        Duration::from_secs(6),
        Duration::from_millis(200),
        || async {
            es_get("test_products", PRODUCT_5).await
                .ok().flatten().is_none()
        },
    ).await;

    watch.abort(); let _ = watch.await;
    assert!(removed, "Deleted product should disappear from ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}

// ── Rapid mutations ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_cdc_rapid_mutations_settle() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;
    let watch = spawn_watch(cfg);

    // Fire 10 rapid updates; final stock = 9 * 10 = 90
    for i in 0u32..10 {
        sqlx::query("UPDATE products SET stock = $1 WHERE id = $2")
            .bind(i as i32 * 10)
            .bind(uuid::Uuid::parse_str(PRODUCT_2)?)
            .execute(&pool).await?;
    }

    let settled = wait_until(
        Duration::from_secs(10),
        Duration::from_millis(300),
        || async {
            es_get("test_products", PRODUCT_2).await
                .ok().flatten()
                .map(|d| d["stock"].as_i64().unwrap_or(-1) == 90)
                .unwrap_or(false)
        },
    ).await;

    watch.abort(); let _ = watch.await;
    assert!(settled, "Final stock value (90) should settle in ES within 10 s");

    reseed(&pool).await?;
    Ok(())
}
