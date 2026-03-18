use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs};

// ────────────────────────────────────────────────────────────────────────────
// Top-level config
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub postgres: PostgresConfig,
    pub elasticsearch: ElasticsearchConfig,
    pub graphql: GraphQLConfig,

    /// One entry per entity you want to expose / index
    #[serde(default)]
    pub entities: Vec<EntityConfig>,
}

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let raw =
            fs::read_to_string(path).with_context(|| format!("Cannot read config file: {path}"))?;
        let cfg: Config =
            serde_yaml::from_str(&raw).with_context(|| format!("Invalid YAML in {path}"))?;
        Ok(cfg)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Sub-configs
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostgresConfig {
    pub url: String,
    /// Max connections in the pool
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
}

fn default_pool_size() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ElasticsearchConfig {
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Cloud ID (alternative to url)
    #[serde(default)]
    pub cloud_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GraphQLConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Expose the GraphiQL playground
    #[serde(default = "bool_true")]
    pub playground: bool,
}

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    4000
}
fn bool_true() -> bool {
    true
}

// ────────────────────────────────────────────────────────────────────────────
// Entity config (the core mapping DSL)
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EntityConfig {
    /// Human name, also used as GraphQL type name
    pub name: String,
    /// Source Postgres table
    pub table: String,
    /// Target ES index name
    pub index: String,
    /// Column used as the ES document `_id`
    #[serde(default = "default_id_col")]
    pub id_column: String,
    /// Postgres channel for LISTEN/NOTIFY CDC  (defaults to table name)
    #[serde(default)]
    pub notify_channel: Option<String>,
    /// Column definitions drive both GQL schema + ES mapping
    pub columns: Vec<ColumnConfig>,
    /// Optional SQL WHERE clause fragment for partial sync
    #[serde(default)]
    pub filter: Option<String>,
    /// Batch size when bulk-indexing
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_id_col() -> String {
    "id".into()
}
fn default_batch_size() -> usize {
    500
}

impl EntityConfig {
    pub fn notify_channel(&self) -> &str {
        self.notify_channel.as_deref().unwrap_or(&self.table)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColumnConfig {
    pub name: String,
    /// Postgres type (used to derive ES mapping if es_type is absent)
    pub pg_type: PgType,
    /// Override the derived ES field type
    #[serde(default)]
    pub es_type: Option<EsFieldType>,
    /// Include a .keyword sub-field for text columns
    #[serde(default = "bool_true")]
    pub keyword_subfield: bool,
    /// Expose in GraphQL schema
    #[serde(default = "bool_true")]
    pub graphql: bool,
    /// Index this field in ES
    #[serde(default = "bool_true")]
    pub indexed: bool,
    /// Custom ES field properties (merged verbatim)
    #[serde(default)]
    pub es_extra: HashMap<String, serde_json::Value>,
}

// ────────────────────────────────────────────────────────────────────────────
// Type enums
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum PgType {
    Uuid,
    Text,
    Varchar,
    Int2,
    Int4,
    Int8,
    Float4,
    Float8,
    Numeric,
    Bool,
    Timestamptz,
    Timestamp,
    Date,
    Jsonb,
    Json,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EsFieldType {
    Keyword,
    Text,
    Integer,
    Long,
    Float,
    Double,
    ScaledFloat,
    Boolean,
    Date,
    Object,
    Nested,
    Ip,
}

impl std::fmt::Display for EsFieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self).unwrap();
        write!(f, "{}", s.as_str().unwrap_or("keyword"))
    }
}
