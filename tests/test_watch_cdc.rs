/// tests/test_watch_cdc.rs
/// Integration tests for `esync watch` — Postgres LISTEN/NOTIFY → ES CDC.
/// All tests are serialised to avoid races on shared DB rows and ES indices.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

async fn setup() -> Result<(sqlx::PgPool, EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;
    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity).await?;
        es_refresh(&entity.index).await?;
    }
    Ok((pool, es, cfg))
}

/// Spawn the watch loop and wait until it has subscribed to all NOTIFY channels
/// before returning. Uses a oneshot channel to signal readiness.
async fn spawn_watch_ready(cfg: Config) -> JoinHandle<()> {
    let (tx, rx) = oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        use esync::commands::watch::WatchArgs;
        use sqlx::postgres::PgListener;

        let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size)
            .await
            .expect("watch: db connect");
        let es = EsClient::new(&cfg.elasticsearch).expect("watch: es client");

        let entities: Vec<_> = cfg.entities.iter().collect();

        let mut listener = PgListener::connect_with(&pool)
            .await
            .expect("watch: PgListener connect");

        for entity in &entities {
            listener
                .listen(entity.notify_channel())
                .await
                .expect("watch: listen");
        }

        // Signal that we are subscribed and ready to receive notifications
        let _ = tx.send(());

        // Now process notifications
        loop {
            let notification = match listener.recv().await {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("listener error: {e}");
                    break;
                }
            };

            let channel = notification.channel();
            let payload = notification.payload();

            let Some(entity) = entities.iter().find(|e| e.notify_channel() == channel) else {
                continue;
            };

            let msg: serde_json::Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("bad payload: {e}");
                    continue;
                }
            };

            let op = msg["op"].as_str().unwrap_or("").to_uppercase();
            let id = msg["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| msg["id"].to_string().trim_matches('"').to_owned());

            match op.as_str() {
                "INSERT" | "UPDATE" => {
                    // Trigger embeds row via row_to_json — use it directly
                    let doc = msg["row"].clone();
                    match es.put_document(&entity.index, &id, doc).await {
                        Ok(_) => tracing::info!("[{}] upsert {id}", entity.index),
                        Err(e) => tracing::error!("[{}] upsert failed: {e}", entity.index),
                    }
                }
                "DELETE" => match es.delete_document(&entity.index, &id).await {
                    Ok(_) => tracing::info!("[{}] delete {id}", entity.index),
                    Err(e) => tracing::error!("[{}] delete failed: {e}", entity.index),
                },
                other => tracing::warn!("unknown op: {other}"),
            }
        }
    });

    // Wait until the listener has subscribed before returning
    let _ = rx.await;
    handle
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_cdc_insert_propagates_to_es() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;
    let watch = spawn_watch_ready(cfg).await; // guaranteed subscribed before INSERT

    let new_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO products (name, price, stock) VALUES ('CDC Widget', 7.77, 10) RETURNING id",
    )
    .fetch_one(&pool)
    .await?;
    let id_str = new_id.to_string();

    let found = wait_until(Duration::from_secs(6), Duration::from_millis(200), || {
        let id = id_str.clone();
        async move { es_get("test_products", &id).await.ok().flatten().is_some() }
    })
    .await;

    watch.abort();
    let _ = watch.await;
    assert!(found, "Inserted product should appear in ES within 6 s");

    let doc = es_get("test_products", &id_str).await?.unwrap();
    assert_eq!(doc["name"], "CDC Widget");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_cdc_update_propagates_to_es() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;
    let watch = spawn_watch_ready(cfg).await;

    sqlx::query("UPDATE products SET price = 99.99, updated_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_1)?)
        .execute(&pool)
        .await?;

    let ok = wait_until(
        Duration::from_secs(6),
        Duration::from_millis(200),
        || async {
            es_get("test_products", PRODUCT_1)
                .await
                .ok()
                .flatten()
                .and_then(|d| {
                    d["price"].as_f64().or_else(|| {
                        // row_to_json may encode numeric as string
                        d["price"].as_str().and_then(|s| s.parse::<f64>().ok())
                    })
                })
                .map(|p| (p - 99.99).abs() < 0.01)
                .unwrap_or(false)
        },
    )
    .await;

    watch.abort();
    let _ = watch.await;
    assert!(ok, "Updated price should reflect in ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_cdc_delete_removes_from_es() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;

    let before = es_get("test_products", PRODUCT_5).await?;
    assert!(before.is_some(), "PRODUCT_5 must exist before delete");

    let watch = spawn_watch_ready(cfg).await;

    sqlx::query("DELETE FROM products WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_5)?)
        .execute(&pool)
        .await?;

    let removed = wait_until(
        Duration::from_secs(6),
        Duration::from_millis(200),
        || async {
            es_get("test_products", PRODUCT_5)
                .await
                .ok()
                .flatten()
                .is_none()
        },
    )
    .await;

    watch.abort();
    let _ = watch.await;
    assert!(
        removed,
        "Deleted product should disappear from ES within 6 s"
    );

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_cdc_rapid_mutations_settle() -> Result<()> {
    let (pool, _es, cfg) = setup().await?;
    let watch = spawn_watch_ready(cfg).await;

    // 10 rapid updates; final stock = 9 * 10 = 90
    for i in 0u32..10 {
        sqlx::query("UPDATE products SET stock = $1 WHERE id = $2")
            .bind(i as i32 * 10)
            .bind(uuid::Uuid::parse_str(PRODUCT_2)?)
            .execute(&pool)
            .await?;
    }

    let settled = wait_until(
        Duration::from_secs(10),
        Duration::from_millis(300),
        || async {
            es_get("test_products", PRODUCT_2)
                .await
                .ok()
                .flatten()
                .map(|d| {
                    // stock may come as JSON number or string depending on path
                    d["stock"]
                        .as_i64()
                        .or_else(|| d["stock"].as_str().and_then(|s| s.parse().ok()))
                        .unwrap_or(-1)
                        == 90
                })
                .unwrap_or(false)
        },
    )
    .await;

    watch.abort();
    let _ = watch.await;
    assert!(settled, "Final stock (90) should settle in ES within 10 s");

    reseed(&pool).await?;
    Ok(())
}
