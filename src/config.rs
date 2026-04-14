use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs};

// ── Top-level config ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub postgres: PostgresConfig,
    pub elasticsearch: ElasticsearchConfig,
    pub graphql: GraphQLConfig,

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

    /// Look up an entity by name.
    pub fn entity(&self, name: &str) -> Option<&EntityConfig> {
        self.entities.iter().find(|e| e.name == name)
    }
}

// ── Sub-configs ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostgresConfig {
    pub url: String,
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
    #[serde(default)]
    pub cloud_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GraphQLConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "bool_true")]
    pub playground: bool,
    /// Optional OAuth2 / JWT authentication configuration.
    #[serde(default)]
    pub oauth2: Option<OAuth2Config>,
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

// ── OAuth2 / JWT config ───────────────────────────────────────────────────

/// How tokens should be validated.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMode {
    /// Validate JWT locally using JWKS (default).
    #[default]
    Jwks,
    /// Call RFC 7662 token introspection endpoint.
    Introspect,
    /// Skip validation — DEV / TEST only. Never use in production.
    None,
}

/// Which token type claim to use for client-credential detection.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RlsTokenType {
    #[default]
    Auto,
    ClientCredentials,
    User,
}

/// Full OAuth2 authentication + RLS configuration block.
///
/// # Minimal YAML example
/// ```yaml
/// graphql:
///   oauth2:
///     validation_mode: jwks
///     jwks_uri: "https://auth.example.com/.well-known/jwks.json"
///     required_issuer: "https://auth.example.com/"
///     required_audience: "esync-api"
///     require_auth: true
///     # Policies use request.jwt.claims ::jsonb — no per-claim config needed.
///       - sub
///       - tenant_id
///       - email
/// ```
///
/// # Full options
/// ```yaml
/// graphql:
///   oauth2:
///     validation_mode: introspect    # jwks | introspect | none
///     introspect_endpoint: "https://auth.example.com/oauth/introspect"
///     client_id: "esync-service"
///     client_secret: "${OAUTH2_CLIENT_SECRET}"
///     jwks_uri: "https://auth.example.com/.well-known/jwks.json"
///     jwks_cache_ttl_secs: 300
///     required_issuer: "https://auth.example.com/"
///     required_audience: "esync-api"
///     clock_skew_secs: 30
///     require_auth: false            # allow anonymous when no Authorization header
///     # Policies use request.jwt.claims ::jsonb — no per-claim config needed.
///       - sub
///       - tenant_id
///       - email
///       - department
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OAuth2Config {
    /// Token validation strategy.
    #[serde(default)]
    pub validation_mode: ValidationMode,

    // ── JWKS ─────────────────────────────────────────────────────────────
    /// JWKS endpoint URI for `validation_mode: jwks`.
    #[serde(default)]
    pub jwks_uri: Option<String>,
    /// How long to cache the JWKS response in seconds (default 300).
    #[serde(default)]
    pub jwks_cache_ttl_secs: Option<u64>,

    // ── Introspection ─────────────────────────────────────────────────────
    /// RFC 7662 introspection endpoint for `validation_mode: introspect`.
    #[serde(default)]
    pub introspect_endpoint: Option<String>,
    /// Client ID used for introspection endpoint basic-auth.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Client secret used for introspection endpoint basic-auth.
    #[serde(default)]
    pub client_secret: Option<String>,

    // ── Standard claim validation ─────────────────────────────────────────
    /// Expected `iss` claim value.  Omit to skip issuer validation.
    #[serde(default)]
    pub required_issuer: Option<String>,
    /// Expected `aud` claim value.  Omit to skip audience validation.
    #[serde(default)]
    pub required_audience: Option<String>,
    /// Allowed clock skew in seconds for `nbf` / `exp` checks (default 30).
    #[serde(default)]
    pub clock_skew_secs: Option<u64>,

    // ── Access control ────────────────────────────────────────────────────
    /// When true, requests without an Authorization header are rejected (401).
    /// When false (default), they proceed as anonymous with no RLS vars set.
    #[serde(default)]
    pub require_auth: bool,

    // ── RLS — JWT claims injection ────────────────────────────────────────
    /// GUC parameter name that receives the full JWT payload as a JSON string.
    /// Default: `"request.jwt.claims"` (PostgREST-compatible).
    ///
    /// Postgres policies use `::jsonb` to query any claim directly:
    ///
    /// ```sql
    /// -- Any claim
    /// current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
    /// current_setting('request.jwt.claims', true)::jsonb ->> 'azp'
    ///
    /// -- Keycloak nested roles (array contains check)
    /// current_setting('request.jwt.claims', true)::jsonb
    ///   -> 'realm_access' -> 'roles' ? 'admin'
    ///
    /// -- Custom claim
    /// current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
    /// ```
    ///
    /// Set to `""` to disable GUC injection entirely (not recommended).
    #[serde(default = "default_jwt_claims_param")]
    pub jwt_claims_param: String,
}

fn default_jwt_claims_param() -> String {
    "request.jwt.claims".into()
}

// ── Entity config ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EntityConfig {
    /// GraphQL type name (CamelCase recommended)
    pub name: String,
    /// Source Postgres table — required unless `sql` is set.
    /// When `sql` is set this is only used as the CDC notify channel default
    /// and as the write target for mutations; leave it empty to make the
    /// entity fully read-only (no CDC, no mutations).
    #[serde(default)]
    pub table: String,
    /// Optional custom SQL query used as the data source instead of a plain
    /// table scan.  The query must select an `id_column` value.
    /// Entities with `sql` set are implicitly read-only (mutations are
    /// blocked) unless you also set `readonly: false` explicitly — but that
    /// combination makes no sense for most views, so omitting it is fine.
    ///
    /// Example:
    /// ```yaml
    /// sql: |
    ///   SELECT p.id, p.name, c.name AS category_name,
    ///          COUNT(o.id) AS order_count
    ///   FROM products p
    ///   JOIN categories c ON c.id = p.category_id
    ///   LEFT JOIN orders o ON o.product_id = p.id
    ///   GROUP BY p.id, p.name, c.name
    /// ```
    #[serde(default)]
    pub sql: Option<String>,
    /// Explicitly mark this entity as read-only.  When true, `create_*`,
    /// `update_*`, and `delete_*` mutations are not generated.
    /// Automatically true when `sql` is set.
    #[serde(default)]
    pub readonly: bool,
    /// Target ES index name
    pub index: String,
    /// Column used as the ES document `_id`
    #[serde(default = "default_id_col")]
    pub id_column: String,
    /// Postgres LISTEN/NOTIFY channel for CDC (defaults to table name)
    #[serde(default)]
    pub notify_channel: Option<String>,
    /// Column definitions — drive both GQL schema and ES mapping
    pub columns: Vec<ColumnConfig>,
    /// Relationships to other entities (no FK required)
    #[serde(default)]
    pub relations: Vec<RelationConfig>,
    /// Optional SQL WHERE fragment applied to all operations
    #[serde(default)]
    pub filter: Option<String>,
    /// Rows per bulk-index request
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Computed denormalized text field built from own columns + relations,
    /// stored in ES for full-text search.
    #[serde(default)]
    pub search_text: Option<SearchTextConfig>,
    /// ES-backed full-text search configuration
    #[serde(default)]
    pub search: SearchConfig,
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

    /// Returns true if mutations should be blocked for this entity.
    /// An entity is read-only when `readonly: true` OR when a custom `sql`
    /// source is configured (arbitrary SQL cannot be written back to).
    pub fn is_readonly(&self) -> bool {
        self.readonly || self.sql.is_some()
    }

    /// The SQL fragment used as the row source in SELECT statements.
    /// For sql-backed entities this wraps the custom query as a sub-select;
    /// for table-backed entities it is just the table name.
    pub fn source_sql(&self) -> String {
        match &self.sql {
            Some(query) => format!("({}) AS _src", query.trim()),
            None => self.table.clone(),
        }
    }
}

// ── Relation config ───────────────────────────────────────────────────────

/// Describes how one entity relates to another.
///
/// # Examples (YAML)
///
/// ## belongs_to  (many-to-one — returns a single nullable object)
/// ```yaml
/// relations:
///   - field: customer          # GQL field name on this type
///     kind: belongs_to
///     target: Customer         # entity name in `entities:`
///     local_col: customer_id   # column on THIS table
///     foreign_col: id          # column on the TARGET table (default: id)
/// ```
///
/// ## has_many  (one-to-many — returns a list)
/// ```yaml
/// relations:
///   - field: orders
///     kind: has_many
///     target: Order
///     local_col: id
///     foreign_col: customer_id
///     limit: 50                # optional cap (default: 100)
///     order_by: placed_at DESC # optional ORDER BY clause
///     filter: status = 'active' # optional extra WHERE fragment
/// ```
///
/// ## many_to_many  (join table — returns a list)
/// ```yaml
/// relations:
///   - field: tags
///     kind: many_to_many
///     target: Tag
///     join_table: product_tags
///     local_col: product_id      # join_table column pointing to THIS entity
///     foreign_col: tag_id        # join_table column pointing to TARGET entity
///     target_id_col: id          # pk on target table (default: id)
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RelationConfig {
    /// The GraphQL field name this relation is exposed under
    pub field: String,
    /// Relationship cardinality
    pub kind: RelationKind,
    /// Name of the target EntityConfig
    pub target: String,
    /// Column on the "local" side (this entity's table, or join table)
    pub local_col: String,
    /// Column on the "foreign" side (target entity's table, or join table)
    #[serde(default = "default_id_col")]
    pub foreign_col: String,

    // ── many_to_many extras ───────────────────────────────────────────────
    /// Join table name (many_to_many only)
    #[serde(default)]
    pub join_table: Option<String>,
    /// PK column on the target table (many_to_many, default "id")
    #[serde(default = "default_id_col")]
    pub target_id_col: String,
    /// Actual Postgres table name for the target entity.
    /// If unset, the `target` entity name is used as the table name.
    /// Set this when the entity name differs from the table name.
    #[serde(default)]
    pub target_table: Option<String>,

    // ── has_many / many_to_many list controls ────────────────────────────
    /// Max rows returned (default 100)
    #[serde(default = "default_relation_limit")]
    pub limit: i64,
    /// Optional ORDER BY clause fragment, e.g. "placed_at DESC"
    #[serde(default)]
    pub order_by: Option<String>,
    /// Optional extra WHERE fragment applied to the related rows
    #[serde(default)]
    pub filter: Option<String>,
}

fn default_relation_limit() -> i64 {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Many rows on this side → one row on the other (e.g. order.customer)
    BelongsTo,
    /// One row on this side → many rows on the other (e.g. customer.orders)
    HasMany,
    /// Many-to-many via a join table
    ManyToMany,
}

// ── search_text denormalization config ───────────────────────────────────

/// Defines a computed `search_text` field that gets built during indexing
/// by joining data from the entity's own columns and its relations.
/// The resulting string is stored in Elasticsearch for full-text search.
///
/// # Example (YAML)
/// ```yaml
/// search_text:
///   field: search_text        # ES field name (default: "search_text")
///   separator: " "            # join separator (default: " ")
///   sources:
///     - column: name          # own column
///     - column: description
///     - relation: category    # belongs_to → pulls category.name, category.slug
///       columns: [name, slug]
///     - relation: tags        # has_many / many_to_many → all tag.label values joined
///       columns: [label]
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SearchTextConfig {
    /// Name of the ES field to store the concatenated text (default: "search_text")
    #[serde(default = "default_search_text_field")]
    pub field: String,

    /// String used to join all parts together (default: single space)
    #[serde(default = "default_separator")]
    pub separator: String,

    /// Ordered list of sources to include in the text
    #[serde(default)]
    pub sources: Vec<SearchTextSource>,
}

fn default_search_text_field() -> String {
    "search_text".into()
}
fn default_separator() -> String {
    " ".into()
}

/// One source contributing text to the `search_text` field.
/// Exactly one of `column` or `relation` must be set.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchTextSource {
    /// Name of a column on this entity's own table
    #[serde(default)]
    pub column: Option<String>,

    /// Name of a relation defined in `relations:` — pulls text from the related table
    #[serde(default)]
    pub relation: Option<String>,

    /// For relation sources: which columns on the related table to include
    #[serde(default)]
    pub columns: Vec<String>,
}

// ── Search config ─────────────────────────────────────────────────────────

/// Opt-in ES-backed search for an entity.
/// Adds a `search_<entity>` query to the GraphQL schema.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SearchConfig {
    /// Whether to expose a `search_*` query for this entity
    #[serde(default)]
    pub enabled: bool,

    /// ES fields to search across, with optional boosts  (e.g. "name^3")
    #[serde(default)]
    pub fields: Vec<SearchField>,

    /// ES field names to include highlighted snippets for
    #[serde(default)]
    pub highlight: Vec<String>,

    /// After ES search, enrich hits with these relation names (defined in `relations:`)
    #[serde(default)]
    pub enrich: Vec<String>,

    /// Columns to re-fetch live from Postgres (always fresh, bypasses ES _source staleness)
    #[serde(default)]
    pub live_columns: Vec<String>,

    /// Extra ES index names to search together with this entity's index (cross-index search)
    #[serde(default)]
    pub cross_index: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchField {
    /// ES field path, with optional boost suffix  e.g. "name^3"
    pub field: String,
}

impl SearchField {
    /// Returns (field_path, boost_factor)
    pub fn parse(&self) -> (&str, Option<f32>) {
        if let Some((name, boost)) = self.field.split_once('^') {
            let b: Option<f32> = boost.parse().ok();
            (name, b)
        } else {
            (self.field.as_str(), None)
        }
    }
}

// ── Column config ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColumnConfig {
    pub name: String,
    pub pg_type: PgType,
    #[serde(default)]
    pub es_type: Option<EsFieldType>,
    #[serde(default = "bool_true")]
    pub keyword_subfield: bool,
    #[serde(default = "bool_true")]
    pub graphql: bool,
    #[serde(default = "bool_true")]
    pub indexed: bool,
    #[serde(default)]
    pub es_extra: HashMap<String, serde_json::Value>,
}

// ── Type enums ────────────────────────────────────────────────────────────

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
