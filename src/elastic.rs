use anyhow::{bail, Result};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use crate::config::ElasticsearchConfig;

pub struct EsClient {
    http: Client,
    base: String,
    auth: Option<(String, String)>,
}

impl EsClient {
    /// Create a no-op client — used when no search-enabled entities exist.
    /// All requests on this client will fail gracefully (caller ignores them).
    pub fn new_noop() -> Self {
        Self {
            http: Client::builder().build().expect("http client"),
            base: "http://localhost:9200".to_string(),
            auth: None,
        }
    }

    pub fn new(cfg: &ElasticsearchConfig) -> Result<Self> {
        let http = Client::builder()
            .danger_accept_invalid_certs(false) // set true for dev self-signed
            .build()?;

        let auth = match (&cfg.username, &cfg.password) {
            (Some(u), Some(p)) => Some((u.clone(), p.clone())),
            _ => None,
        };

        Ok(Self {
            http,
            base: cfg.url.trim_end_matches('/').to_string(),
            auth,
        })
    }

    // ── generic helpers ───────────────────────────────────────────────────

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let mut r = self.http.request(method, &url);
        if let Some((u, p)) = &self.auth {
            r = r.basic_auth(u, Some(p));
        }
        r
    }

    async fn check(resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(json!({}));
        if !status.is_success() {
            bail!("ES error {status}: {body}");
        }
        Ok(body)
    }

    // ── Index CRUD ────────────────────────────────────────────────────────

    pub async fn index_exists(&self, index: &str) -> Result<bool> {
        let resp = self.req(reqwest::Method::HEAD, index).send().await?;
        Ok(resp.status() == StatusCode::OK)
    }

    pub async fn create_index(&self, index: &str, body: Value) -> Result<Value> {
        let resp = self
            .req(reqwest::Method::PUT, index)
            .json(&body)
            .send()
            .await?;
        Self::check(resp).await
    }

    pub async fn delete_index(&self, index: &str) -> Result<Value> {
        let resp = self.req(reqwest::Method::DELETE, index).send().await?;
        Self::check(resp).await
    }

    pub async fn recreate_index(&self, index: &str, body: Value) -> Result<()> {
        if self.index_exists(index).await? {
            tracing::info!("Deleting existing index `{index}`");
            self.delete_index(index).await?;
        }
        self.create_index(index, body).await?;
        tracing::info!("Created index `{index}`");
        Ok(())
    }

    pub async fn get_index(&self, index: &str) -> Result<Value> {
        let resp = self.req(reqwest::Method::GET, index).send().await?;
        Self::check(resp).await
    }

    pub async fn list_indices(&self, pattern: &str) -> Result<Value> {
        let path = format!("_cat/indices/{pattern}?format=json&v");
        let resp = self.req(reqwest::Method::GET, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn index_mappings(&self, index: &str) -> Result<Value> {
        let path = format!("{index}/_mapping");
        let resp = self.req(reqwest::Method::GET, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn put_mapping(&self, index: &str, body: Value) -> Result<Value> {
        let path = format!("{index}/_mapping");
        let resp = self
            .req(reqwest::Method::PUT, &path)
            .json(&body)
            .send()
            .await?;
        Self::check(resp).await
    }

    // ── Document CRUD ─────────────────────────────────────────────────────

    pub async fn put_document(&self, index: &str, id: &str, doc: Value) -> Result<Value> {
        let path = format!("{index}/_doc/{id}");
        let resp = self
            .req(reqwest::Method::PUT, &path)
            .json(&doc)
            .send()
            .await?;
        Self::check(resp).await
    }

    pub async fn get_document(&self, index: &str, id: &str) -> Result<Value> {
        let path = format!("{index}/_doc/{id}");
        let resp = self.req(reqwest::Method::GET, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn delete_document(&self, index: &str, id: &str) -> Result<Value> {
        let path = format!("{index}/_doc/{id}");
        let resp = self.req(reqwest::Method::DELETE, &path).send().await?;
        Self::check(resp).await
    }

    /// POST /<index>/_search with arbitrary body
    pub async fn search(&self, index: &str, query: Value) -> Result<Value> {
        let path = format!("{index}/_search");
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(&query)
            .send()
            .await?;
        Self::check(resp).await
    }

    // ── Bulk ──────────────────────────────────────────────────────────────

    pub async fn bulk_index(&self, index: &str, docs: &[(String, Value)]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }

        let mut ndjson = String::new();
        for (id, doc) in docs {
            ndjson.push_str(&format!(
                "{{\"index\":{{\"_index\":\"{index}\",\"_id\":\"{id}\"}}}}\n"
            ));
            ndjson.push_str(&serde_json::to_string(doc)?);
            ndjson.push('\n');
        }

        let resp = self
            .req(reqwest::Method::POST, "_bulk")
            .header("Content-Type", "application/x-ndjson")
            .body(ndjson)
            .send()
            .await?;

        let body: Value = resp.json().await?;
        if body["errors"].as_bool().unwrap_or(false) {
            // Collect the first failed item's reason for a useful error message
            let reason = body["items"]
                .as_array()
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        let op = item.get("index").or_else(|| item.get("create"))?;
                        if op["status"].as_i64().unwrap_or(200) >= 400 {
                            op["error"]["reason"].as_str().map(str::to_owned)
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_else(|| "unknown bulk error".to_string());
            anyhow::bail!("Bulk indexing failed: {reason}");
        }
        Ok(())
    }

    // ── Data Streams ──────────────────────────────────────────────────────

    pub async fn create_datastream(&self, name: &str) -> Result<Value> {
        let path = format!("_data_stream/{name}");
        let resp = self.req(reqwest::Method::PUT, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn delete_datastream(&self, name: &str) -> Result<Value> {
        let path = format!("_data_stream/{name}");
        let resp = self.req(reqwest::Method::DELETE, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn list_datastreams(&self, pattern: &str) -> Result<Value> {
        let path = format!("_data_stream/{pattern}");
        let resp = self.req(reqwest::Method::GET, &path).send().await?;
        Self::check(resp).await
    }

    // ── Index Templates ───────────────────────────────────────────────────

    pub async fn put_template(&self, name: &str, body: Value) -> Result<Value> {
        let path = format!("_index_template/{name}");
        let resp = self
            .req(reqwest::Method::PUT, &path)
            .json(&body)
            .send()
            .await?;
        Self::check(resp).await
    }

    pub async fn get_template(&self, name: &str) -> Result<Value> {
        let path = format!("_index_template/{name}");
        let resp = self.req(reqwest::Method::GET, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn delete_template(&self, name: &str) -> Result<Value> {
        let path = format!("_index_template/{name}");
        let resp = self.req(reqwest::Method::DELETE, &path).send().await?;
        Self::check(resp).await
    }

    // ── ILM Policies ─────────────────────────────────────────────────────

    pub async fn put_policy(&self, name: &str, body: Value) -> Result<Value> {
        let path = format!("_ilm/policy/{name}");
        let resp = self
            .req(reqwest::Method::PUT, &path)
            .json(&body)
            .send()
            .await?;
        Self::check(resp).await
    }

    pub async fn get_policy(&self, name: &str) -> Result<Value> {
        let path = format!("_ilm/policy/{name}");
        let resp = self.req(reqwest::Method::GET, &path).send().await?;
        Self::check(resp).await
    }

    pub async fn delete_policy(&self, name: &str) -> Result<Value> {
        let path = format!("_ilm/policy/{name}");
        let resp = self.req(reqwest::Method::DELETE, &path).send().await?;
        Self::check(resp).await
    }
}
