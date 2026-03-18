/// tests/test_elastic.rs
/// Integration tests for src/elastic.rs.
/// Each test creates and cleans up its own index so tests can run in parallel.
/// Requires: Elasticsearch on http://localhost:9200 (no auth).

mod common;
use common::*;

use anyhow::Result;
use esync::{config::ElasticsearchConfig, elastic::EsClient};
use serde_json::json;

fn es_cfg() -> ElasticsearchConfig {
    ElasticsearchConfig {
        url:      ES_URL.to_string(),
        username: None,
        password: None,
        cloud_id: None,
    }
}

fn basic_body() -> serde_json::Value {
    json!({ "settings": { "number_of_shards": 1, "number_of_replicas": 0 } })
}

// ── Index lifecycle ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_delete_index() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_create_delete";
    let _   = es.delete_index(idx).await; // clean slate

    es.create_index(idx, basic_body()).await?;
    assert!(es.index_exists(idx).await?, "Index must exist after create");

    es.delete_index(idx).await?;
    assert!(!es.index_exists(idx).await?, "Index must be gone after delete");
    Ok(())
}

#[tokio::test]
async fn test_recreate_index_drops_existing_data() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_recreate";
    let _   = es.delete_index(idx).await;

    es.create_index(idx, basic_body()).await?;
    es.put_document(idx, "doc1", json!({ "hello": "world" })).await?;
    es_refresh(idx).await?;
    assert_eq!(es_all(idx).await?.len(), 1, "1 doc before recreate");

    es.recreate_index(idx, basic_body()).await?;
    es_refresh(idx).await?;
    assert_eq!(es_all(idx).await?.len(), 0, "0 docs after recreate");

    es.delete_index(idx).await?;
    Ok(())
}

#[tokio::test]
async fn test_get_index_returns_metadata() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_get_index";
    let _   = es.delete_index(idx).await;

    es.create_index(idx, json!({
        "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
        "mappings": { "properties": { "title": { "type": "keyword" } } }
    })).await?;

    let info = es.get_index(idx).await?;
    assert!(info.get(idx).is_some(), "Response must contain the index name as key");

    es.delete_index(idx).await?;
    Ok(())
}

// ── Document CRUD ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_put_and_get_document() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_doc_crud";
    let _   = es.delete_index(idx).await;
    es.create_index(idx, basic_body()).await?;

    let doc = json!({ "name": "Test Item", "price": 9.99, "active": true });
    es.put_document(idx, "abc-123", doc).await?;

    let fetched = es.get_document(idx, "abc-123").await?;
    assert_eq!(fetched["_id"],             "abc-123");
    assert_eq!(fetched["_source"]["name"], "Test Item");

    es.delete_index(idx).await?;
    Ok(())
}

#[tokio::test]
async fn test_delete_document() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_doc_delete";
    let _   = es.delete_index(idx).await;
    es.create_index(idx, basic_body()).await?;

    es.put_document(idx, "to-delete", json!({ "x": 1 })).await?;
    es.delete_document(idx, "to-delete").await?;

    // ES returns HTTP 404 when the doc is gone; our client wraps that as Err
    let result = es.get_document(idx, "to-delete").await;
    assert!(
        result.is_err() || result.unwrap()["found"] == json!(false),
        "Document should not be found after delete"
    );

    es.delete_index(idx).await?;
    Ok(())
}

// ── Bulk indexing ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_bulk_index_25_documents() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_bulk";
    let _   = es.delete_index(idx).await;
    es.create_index(idx, basic_body()).await?;

    let docs: Vec<(String, serde_json::Value)> = (1..=25)
        .map(|i| (format!("doc-{i:03}"), json!({ "seq": i, "label": format!("item {i}") })))
        .collect();

    es.bulk_index(idx, &docs).await?;
    es_refresh(idx).await?;

    let hits = es_all(idx).await?;
    assert_eq!(hits.len(), 25, "All 25 docs should be indexed");

    es.delete_index(idx).await?;
    Ok(())
}

#[tokio::test]
async fn test_bulk_index_empty_is_noop() -> Result<()> {
    let es = EsClient::new(&es_cfg())?;
    // Empty slice must not error (no HTTP request is made)
    es.bulk_index("nonexistent_index", &[]).await?;
    Ok(())
}

// ── Search ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_term_query_returns_matches() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_search";
    let _   = es.delete_index(idx).await;
    es.create_index(idx, json!({
        "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
        "mappings": { "properties": {
            "category": { "type": "keyword" },
            "value":    { "type": "integer" }
        }}
    })).await?;

    let docs = vec![
        ("1".into(), json!({ "category": "A", "value": 10 })),
        ("2".into(), json!({ "category": "B", "value": 20 })),
        ("3".into(), json!({ "category": "A", "value": 30 })),
    ];
    es.bulk_index(idx, &docs).await?;
    es_refresh(idx).await?;

    let result = es.search(idx, json!({
        "query": { "term": { "category": "A" } }
    })).await?;

    let count = result["hits"]["total"]["value"].as_i64().unwrap_or(0);
    assert_eq!(count, 2, "Expected 2 docs with category=A");

    es.delete_index(idx).await?;
    Ok(())
}

// ── Mappings ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_put_and_get_mappings() -> Result<()> {
    let es  = EsClient::new(&es_cfg())?;
    let idx = "it_mappings";
    let _   = es.delete_index(idx).await;
    es.create_index(idx, json!({
        "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
        "mappings": { "properties": { "title": { "type": "text" } } }
    })).await?;

    es.put_mapping(idx, json!({
        "properties": { "score": { "type": "float" } }
    })).await?;

    let m = es.index_mappings(idx).await?;
    let props = &m[idx]["mappings"]["properties"];
    assert!(props.get("title").is_some(), "title field should be present");
    assert!(props.get("score").is_some(), "score field should appear after put_mapping");

    es.delete_index(idx).await?;
    Ok(())
}

// ── Templates ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_index_template_lifecycle() -> Result<()> {
    let es   = EsClient::new(&es_cfg())?;
    let name = "it-test-template";
    let _    = es.delete_template(name).await;

    es.put_template(name, json!({
        "index_patterns": ["it-test-*"],
        "template": {
            "settings": { "number_of_shards": 1 },
            "mappings": { "properties": { "ts": { "type": "date" } } }
        }
    })).await?;

    let fetched = es.get_template(name).await?;
    let names: Vec<&str> = fetched["index_templates"]
        .as_array().unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&name), "Template must appear after put");

    es.delete_template(name).await?;
    assert!(es.get_template(name).await.is_err(), "Template must be gone after delete");
    Ok(())
}

// ── ILM Policies ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_ilm_policy_lifecycle() -> Result<()> {
    let es   = EsClient::new(&es_cfg())?;
    let name = "it-test-policy";
    let _    = es.delete_policy(name).await;

    es.put_policy(name, json!({
        "policy": {
            "phases": {
                "hot":    { "min_age": "0ms",  "actions": { "rollover": { "max_age": "7d" } } },
                "delete": { "min_age": "30d",  "actions": { "delete": {} } }
            }
        }
    })).await?;

    let fetched = es.get_policy(name).await?;
    assert!(fetched.get(name).is_some(), "Policy must appear after put");

    es.delete_policy(name).await?;
    assert!(es.get_policy(name).await.is_err(), "Policy must be gone after delete");
    Ok(())
}
