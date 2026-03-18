/// tests/test_search_text.rs
/// Integration tests for the search_text denormalization pipeline.
///
/// Verifies that after `rebuild_index`:
///   - the `search_text` field is present in ES documents
///   - own columns are included in the text
///   - relation columns (belongs_to, has_many, many_to_many) are included
///   - CDC watch rebuilds search_text correctly after a PG change
///   - the field is searchable via a plain ES match query
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db, elastic::EsClient, indexer};
use serial_test::serial;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

// ── Setup ─────────────────────────────────────────────────────────────────

async fn setup() -> Result<(sqlx::PgPool, EsClient, Config)> {
    let cfg = Config::load(CFG_PATH)?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;
    reseed(&pool).await?;
    Ok((pool, es, cfg))
}

async fn index_entity(pool: &sqlx::PgPool, es: &EsClient, cfg: &Config, name: &str) -> Result<()> {
    let entity = cfg
        .entities
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow::anyhow!("entity {name} not found"))?;
    es_delete_index(&entity.index).await?;
    indexer::rebuild_index(pool, es, entity, cfg).await?;
    es_refresh(&entity.index).await?;
    Ok(())
}

// ── Own column sources ────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_text_own_columns_present() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    index_entity(&pool, &es, &cfg, "Product").await?;

    let doc = es.get_document("test_products", PRODUCT_1).await?;
    let src = &doc["_source"];

    assert!(
        !src["search_text"].is_null(),
        "search_text field must be present in ES document"
    );

    let text = src["search_text"].as_str().unwrap_or("");
    // Product 1 is "Alpha Widget" with description "First widget"
    assert!(
        text.contains("Alpha Widget"),
        "search_text must include product name"
    );
    assert!(
        text.contains("First widget"),
        "search_text must include description"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_text_null_columns_omitted() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    // PRODUCT_4 has description = NULL in the seed data
    sqlx::query("UPDATE products SET description = NULL WHERE id = $1")
        .bind(uuid::Uuid::parse_str(PRODUCT_4)?)
        .execute(&pool)
        .await?;

    index_entity(&pool, &es, &cfg, "Product").await?;

    let doc = es.get_document("test_products", PRODUCT_4).await?;
    let text = doc["_source"]["search_text"].as_str().unwrap_or("");
    assert!(
        !text.is_empty(),
        "search_text should still have name even if description is null"
    );
    // Should not have a dangling separator from the null description
    assert!(!text.starts_with(' '), "no leading space");
    assert!(!text.ends_with(' '), "no trailing space");
    assert!(!text.contains("  "), "no double spaces from null gaps");

    reseed(&pool).await?;
    Ok(())
}

// ── Relation sources ──────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_text_many_to_many_relation_included() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    // Customer entity has search_text with tags (many_to_many) as a source
    // Alice has tags: "vip" and "wholesale"
    index_entity(&pool, &es, &cfg, "Customer").await?;

    let doc = es.get_document("test_customers", CUSTOMER_ALICE).await?;
    let text = doc["_source"]["search_text"].as_str().unwrap_or("");

    assert!(
        text.contains("Alice"),
        "search_text must include customer name"
    );
    assert!(
        text.contains("vip"),
        "search_text must include many_to_many tag label"
    );
    assert!(
        text.contains("wholesale"),
        "search_text must include all tag labels"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_text_has_many_relation_included() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    // Customer has search_text with orders (has_many) → status column
    index_entity(&pool, &es, &cfg, "Customer").await?;

    let doc = es.get_document("test_customers", CUSTOMER_ALICE).await?;
    let text = doc["_source"]["search_text"].as_str().unwrap_or("");

    // Alice has orders with status "completed", "pending", "cancelled"
    assert!(
        text.contains("completed") || text.contains("pending") || text.contains("cancelled"),
        "search_text must include has_many relation values, got: '{text}'"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_text_no_relation_data_for_empty_customer() -> Result<()> {
    let (pool, es, cfg) = setup().await?;

    // Bob has no tags and no orders
    index_entity(&pool, &es, &cfg, "Customer").await?;

    let doc = es.get_document("test_customers", CUSTOMER_BOB).await?;
    let text = doc["_source"]["search_text"].as_str().unwrap_or("");

    assert!(text.contains("Bob"), "Bob's name must be in search_text");
    // Should not contain any tag or order data
    assert!(!text.contains("vip"), "Bob has no vip tag");
    assert!(!text.contains("completed"), "Bob has no orders");

    Ok(())
}

// ── ES full-text searchability ────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_search_text_field_is_searchable_in_es() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    index_entity(&pool, &es, &cfg, "Product").await?;

    // Search the search_text field directly via ES match query
    let result = es
        .search(
            "test_products",
            serde_json::json!({
                "query": { "match": { "search_text": "Widget" } }
            }),
        )
        .await?;

    let count = result["hits"]["total"]["value"].as_i64().unwrap_or(0);
    assert!(
        count >= 1,
        "ES match on search_text should find products containing 'Widget'"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn test_search_text_tag_label_is_searchable() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    index_entity(&pool, &es, &cfg, "Customer").await?;

    // "vip" comes from the many_to_many tags relation, not any own column
    let result = es
        .search(
            "test_customers",
            serde_json::json!({
                "query": { "match": { "search_text": "vip" } }
            }),
        )
        .await?;

    let count = result["hits"]["total"]["value"].as_i64().unwrap_or(0);
    assert_eq!(
        count, 1,
        "Only Alice has the 'vip' tag; search should find exactly 1 customer"
    );

    let hit_id = result["hits"]["hits"][0]["_id"].as_str().unwrap_or("");
    assert_eq!(hit_id, CUSTOMER_ALICE, "The hit must be Alice");

    Ok(())
}

// ── CDC: search_text rebuilt on row change ────────────────────────────────

#[tokio::test]
#[serial]
async fn test_cdc_rebuilds_search_text_on_update() -> Result<()> {
    let (pool, es, cfg) = setup().await?;
    index_entity(&pool, &es, &cfg, "Product").await?;

    // Verify initial state
    let before = es.get_document("test_products", PRODUCT_1).await?;
    let before_text = before["_source"]["search_text"].as_str().unwrap_or("");
    assert!(
        before_text.contains("Alpha Widget"),
        "initial search_text contains product name"
    );

    // Start CDC watch
    let watch = spawn_watch_ready(cfg).await;

    // Update the product name in PG
    sqlx::query(
        "UPDATE products SET name = 'Super Alpha Widget XL', updated_at = NOW() WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(PRODUCT_1)?)
    .execute(&pool)
    .await?;

    // Wait for ES to reflect the new name in search_text
    let updated = wait_until(
        Duration::from_secs(6),
        Duration::from_millis(200),
        || async {
            es_get("test_products", PRODUCT_1)
                .await
                .ok()
                .flatten()
                .map(|d| {
                    d["search_text"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Super Alpha Widget XL")
                })
                .unwrap_or(false)
        },
    )
    .await;

    watch.abort();
    let _ = watch.await;
    assert!(
        updated,
        "CDC must rebuild search_text with new product name within 6 s"
    );

    reseed(&pool).await?;
    Ok(())
}

// ── Helpers (CDC watch with ready signal) ─────────────────────────────────

async fn spawn_watch_ready(cfg: Config) -> JoinHandle<()> {
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        use esync::{db, elastic::EsClient, indexer};
        use sqlx::postgres::PgListener;

        let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size)
            .await
            .expect("watch: db connect");
        let es = EsClient::new(&cfg.elasticsearch).expect("watch: es");
        let entities: Vec<_> = cfg.entities.clone();

        let mut listener = PgListener::connect_with(&pool)
            .await
            .expect("watch: PgListener");
        for entity in &entities {
            listener
                .listen(entity.notify_channel())
                .await
                .expect("watch: listen");
        }
        let _ = tx.send(());

        loop {
            let note = match listener.recv().await {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("listener: {e}");
                    break;
                }
            };
            let channel = note.channel();
            let payload = note.payload();

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
                    let mut doc = msg["row"].clone();
                    if doc.is_null() {
                        continue;
                    }

                    // Rebuild search_text fresh from PG
                    if entity.search_text.is_some() {
                        if let Ok(Some(text)) =
                            indexer::build_search_text_for_id(&pool, entity, &id, &cfg).await
                        {
                            let field = entity
                                .search_text
                                .as_ref()
                                .map(|c| c.field.as_str())
                                .unwrap_or("search_text");
                            doc[field] = serde_json::Value::String(text);
                        }
                    }

                    let _ = es.put_document(&entity.index, &id, doc).await;
                }
                "DELETE" => {
                    let _ = es.delete_document(&entity.index, &id).await;
                }
                _ => {}
            }
        }
    });
    let _ = rx.await;
    handle
}
