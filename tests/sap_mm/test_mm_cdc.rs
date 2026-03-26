/// tests/sap_mm/test_mm_cdc.rs
/// Integration tests for `esync watch` CDC against the SAP MM schema.
/// Covers material_master and vendor_master — the two tables with NOTIFY triggers.
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;
use sqlx::postgres::PgListener;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

// ── Setup ─────────────────────────────────────────────────────────────────────

async fn setup() -> Result<(sqlx::PgPool, EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;
    for entity in &cfg.entities {
        indexer::rebuild_index(&pool, &es, entity, &cfg).await?;
        es_refresh(&entity.index).await?;
    }
    Ok((pool, es, cfg))
}

/// Spawn a minimal CDC listener that covers only the channels with NOTIFY triggers
/// (mm_test_material_changes, mm_test_vendor_changes).
/// Uses a oneshot to signal when subscribed, so tests don't race the INSERT.
async fn spawn_cdc(cfg: Config, es: EsClient) -> JoinHandle<()> {
    let (tx, rx) = oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size)
            .await
            .expect("cdc: db connect");

        let mut listener = PgListener::connect_with(&pool)
            .await
            .expect("cdc: listener connect");

        // Only listen on the two channels that have triggers in the test DB
        let watched: Vec<_> = cfg.entities.iter()
            .filter(|e| e.notify_channel.is_some())
            .collect();

        for entity in &watched {
            listener.listen(entity.notify_channel()).await.expect("cdc: listen");
        }

        let _ = tx.send(());  // signal: we are subscribed

        loop {
            let notification = match listener.recv().await {
                Ok(n)  => n,
                Err(e) => { tracing::error!("cdc listener error: {e}"); break; }
            };

            let channel = notification.channel();
            let payload = notification.payload();

            let Some(entity) = watched.iter().find(|e| e.notify_channel() == channel) else {
                continue;
            };

            let msg: serde_json::Value = match serde_json::from_str(payload) {
                Ok(v)  => v,
                Err(e) => { tracing::warn!("bad payload: {e}"); continue; }
            };

            let op = msg["op"].as_str().unwrap_or("").to_uppercase();
            let id = msg["id"].as_str().map(str::to_owned)
                .unwrap_or_else(|| msg["id"].to_string().trim_matches('"').to_owned());

            match op.as_str() {
                "INSERT" | "UPDATE" => {
                    let doc = msg["row"].clone();
                    if let Err(e) = es.put_document(&entity.index, &id, doc).await {
                        tracing::error!("[{}] upsert {id} failed: {e}", entity.index);
                    }
                }
                "DELETE" => {
                    if let Err(e) = es.delete_document(&entity.index, &id).await {
                        tracing::error!("[{}] delete {id} failed: {e}", entity.index);
                    }
                }
                other => tracing::warn!("unknown op: {other}"),
            }
        }
    });

    let _ = rx.await;  // wait until subscribed before returning
    handle
}

// ── Material Master CDC ───────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_cdc_material_insert_propagates() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let cdc = spawn_cdc(cfg, es).await;

    let new_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO material_master (material_number, description, material_type, base_unit)
         VALUES ('MAT-CDC-1', 'CDC Test Material', 'ROH', 'EA') RETURNING id",
    )
    .fetch_one(&pool)
    .await?;
    let id_str = new_id.to_string();

    let found = wait_until(Duration::from_secs(6), Duration::from_millis(200), || {
        let id = id_str.clone();
        async move { es_get(IDX_MATERIAL, &id).await.ok().flatten().is_some() }
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(found, "New material should appear in ES within 6 s");
    let doc = es_get(IDX_MATERIAL, &id_str).await?.unwrap();
    assert_eq!(doc["material_number"], "MAT-CDC-1");
    assert_eq!(doc["material_type"], "ROH");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_cdc_material_update_propagates() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let cdc = spawn_cdc(cfg, es).await;

    sqlx::query("UPDATE material_master SET description = 'Updated Steel Sheet', updated_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(MAT_STEEL)?)
        .execute(&pool)
        .await?;

    let updated = wait_until(Duration::from_secs(6), Duration::from_millis(200), || async {
        es_get(IDX_MATERIAL, MAT_STEEL)
            .await
            .ok()
            .flatten()
            .and_then(|d| d["description"].as_str().map(str::to_owned))
            .map(|desc| desc.contains("Updated"))
            .unwrap_or(false)
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(updated, "Updated material description should propagate to ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_cdc_material_delete_removes_from_es() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    // Confirm it exists first (no FK dependents for MAT_INACT)
    let before = es_get(IDX_MATERIAL, MAT_PUMP).await?;
    assert!(before.is_some(), "MAT-2000 must be in ES before delete");

    let cdc = spawn_cdc(cfg, es).await;

    // Hard delete — remove dependents first
    sqlx::query("DELETE FROM material_document WHERE material_id = $1")
        .bind(uuid::Uuid::parse_str(MAT_PUMP)?)
        .execute(&pool).await?;
    sqlx::query("DELETE FROM purchasing_info WHERE material_id = $1")
        .bind(uuid::Uuid::parse_str(MAT_PUMP)?)
        .execute(&pool).await?;
    sqlx::query("DELETE FROM storage_location WHERE material_id = $1")
        .bind(uuid::Uuid::parse_str(MAT_PUMP)?)
        .execute(&pool).await?;
    sqlx::query("DELETE FROM plant_data WHERE material_id = $1")
        .bind(uuid::Uuid::parse_str(MAT_PUMP)?)
        .execute(&pool).await?;
    sqlx::query("DELETE FROM material_master WHERE id = $1")
        .bind(uuid::Uuid::parse_str(MAT_PUMP)?)
        .execute(&pool).await?;

    let removed = wait_until(Duration::from_secs(6), Duration::from_millis(200), || async {
        es_get(IDX_MATERIAL, MAT_PUMP)
            .await
            .ok()
            .flatten()
            .is_none()
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(removed, "Deleted material should disappear from ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_cdc_material_rapid_updates_settle() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let cdc = spawn_cdc(cfg, es).await;

    // 8 rapid updates; final gross_weight = 8 * 3.0 = 24.0
    for i in 1u32..=8 {
        sqlx::query("UPDATE material_master SET gross_weight = $1, updated_at = NOW() WHERE id = $2")
            .bind(i as f64 * 3.0)
            .bind(uuid::Uuid::parse_str(MAT_BOLT)?)
            .execute(&pool)
            .await?;
    }

    let settled = wait_until(Duration::from_secs(10), Duration::from_millis(300), || async {
        es_get(IDX_MATERIAL, MAT_BOLT)
            .await
            .ok()
            .flatten()
            .and_then(|d| parse_numeric(&d["gross_weight"]))
            .map(|w| (w - 24.0).abs() < 0.01)
            .unwrap_or(false)
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(settled, "Final gross_weight (24.0) should settle in ES within 10 s");

    reseed(&pool).await?;
    Ok(())
}

// ── Vendor Master CDC ─────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_mm_cdc_vendor_insert_propagates() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let cdc = spawn_cdc(cfg, es).await;

    let new_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO vendor_master (vendor_number, name, search_term, country, currency, account_group)
         VALUES ('V999999', 'CDC Test Vendor', 'CDCTEST', 'US', 'USD', 'LIEF') RETURNING id",
    )
    .fetch_one(&pool)
    .await?;
    let id_str = new_id.to_string();

    let found = wait_until(Duration::from_secs(6), Duration::from_millis(200), || {
        let id = id_str.clone();
        async move { es_get(IDX_VENDOR, &id).await.ok().flatten().is_some() }
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(found, "New vendor should appear in ES within 6 s");
    let doc = es_get(IDX_VENDOR, &id_str).await?.unwrap();
    assert_eq!(doc["vendor_number"], "V999999");
    assert_eq!(doc["name"], "CDC Test Vendor");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_cdc_vendor_update_propagates() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    let cdc = spawn_cdc(cfg, es).await;

    sqlx::query("UPDATE vendor_master SET city = 'Chicago', updated_at = NOW() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(VENDOR_ACME)?)
        .execute(&pool)
        .await?;

    let updated = wait_until(Duration::from_secs(6), Duration::from_millis(200), || async {
        es_get(IDX_VENDOR, VENDOR_ACME)
            .await
            .ok()
            .flatten()
            .and_then(|d| d["city"].as_str().map(str::to_owned))
            .map(|city| city == "Chicago")
            .unwrap_or(false)
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(updated, "Vendor city update should propagate to ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_mm_cdc_vendor_delete_removes_from_es() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    let before = es_get(IDX_VENDOR, VENDOR_PACIFIC).await?;
    assert!(before.is_some(), "Pacific vendor must be in ES before delete");

    let cdc = spawn_cdc(cfg, es).await;

    // Pacific has no purchasing_info rows — safe to delete directly
    sqlx::query("DELETE FROM vendor_master WHERE id = $1")
        .bind(uuid::Uuid::parse_str(VENDOR_PACIFIC)?)
        .execute(&pool)
        .await?;

    let removed = wait_until(Duration::from_secs(6), Duration::from_millis(200), || async {
        es_get(IDX_VENDOR, VENDOR_PACIFIC)
            .await
            .ok()
            .flatten()
            .is_none()
    })
    .await;

    cdc.abort();
    let _ = cdc.await;

    assert!(removed, "Deleted vendor should disappear from ES within 6 s");

    reseed(&pool).await?;
    Ok(())
}
