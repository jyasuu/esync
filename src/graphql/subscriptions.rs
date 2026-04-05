//! GraphQL subscription support.
//!
//! Architecture
//! ────────────
//! A `Broadcaster` holds one `tokio::sync::broadcast::Sender<CdcEvent>` per
//! entity (keyed by entity name).  It is created once in `serve.rs` and
//! passed into both `build_schema` and the CDC listener task that runs
//! inside `esync serve`.
//!
//! The CDC listener is a lightweight `PgListener` loop that:
//!   1. Receives a Postgres NOTIFY payload on an entity's channel
//!   2. Deserialises it (same `NotifyPayload` shape as `watch.rs`)
//!   3. Sends a `CdcEvent` to the entity's broadcast channel
//!
//! Each `watch_<entity>` subscription field:
//!   1. Subscribes to the broadcast channel for that entity
//!   2. Streams `CdcEvent` values to the connected WebSocket client
//!
//! The CDC listener is started in `serve.rs` via `spawn_cdc_listener`.

use crate::config::{Config, EntityConfig};
use async_graphql::dynamic::*;
use async_graphql::{Name, Value as GqlValue};
use futures::StreamExt as FuturesStreamExt; // filter_map (async), map, boxed
use serde::Deserialize;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

// ── Event types ───────────────────────────────────────────────────────────

/// One CDC event forwarded to subscription clients.
#[derive(Debug, Clone)]
pub struct CdcEvent {
    pub op: String, // INSERT | UPDATE | DELETE
    pub id: String,
    pub data: Option<serde_json::Value>, // None on DELETE
}

/// Postgres NOTIFY payload shape (mirrors watch.rs).
#[derive(Debug, Deserialize)]
struct NotifyPayload {
    op: String,
    id: serde_json::Value,
    row: Option<serde_json::Value>,
}

// ── Broadcaster ───────────────────────────────────────────────────────────

/// Holds one broadcast channel sender per entity name.
/// Clone-cheap (Arc inside).
#[derive(Clone)]
pub struct Broadcaster {
    inner: Arc<HashMap<String, broadcast::Sender<CdcEvent>>>,
}

impl Broadcaster {
    /// Create a broadcaster with a 256-slot buffer per entity.
    pub fn new(cfg: &Config) -> Self {
        let mut map = HashMap::new();
        for entity in &cfg.entities {
            // sql-backed views have no real table/notify channel
            if entity.sql.is_some() {
                continue;
            }
            let (tx, _) = broadcast::channel(256);
            map.insert(entity.name.clone(), tx);
        }
        Self {
            inner: Arc::new(map),
        }
    }

    /// Get a receiver for `entity_name`.  Returns None if the entity is not
    /// registered (e.g. it is a read-only sql view).
    pub fn subscribe(&self, entity_name: &str) -> Option<broadcast::Receiver<CdcEvent>> {
        self.inner.get(entity_name).map(|tx| tx.subscribe())
    }

    /// Broadcast an event to all subscribers of `entity_name`.
    pub fn send(&self, entity_name: &str, event: CdcEvent) {
        if let Some(tx) = self.inner.get(entity_name) {
            // send() fails only when there are no receivers — that's fine.
            let _ = tx.send(event);
        }
    }
}

// ── CDC listener (runs inside `esync serve`) ──────────────────────────────

/// Spawn a background task that listens on all entity notify channels and
/// forwards CDC events to the broadcaster.
///
/// Returns a `JoinHandle` — the caller should hold it (or just drop it if
/// they want fire-and-forget; the task runs until the pool is dropped).
pub async fn spawn_cdc_listener(
    cfg: Arc<Config>,
    pool: Arc<PgPool>,
    broadcaster: Arc<Broadcaster>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let mut listener = PgListener::connect_with(&pool).await?;

    // Collect all distinct notify channels
    let entities: Vec<_> = cfg
        .entities
        .iter()
        .filter(|e| e.sql.is_none()) // sql-backed views have no notify channel
        .cloned()
        .collect();

    for entity in &entities {
        let channel = entity.notify_channel();
        listener.listen(channel).await?;
        tracing::info!(
            "[subscriptions] listening on channel `{channel}` for entity `{}`",
            entity.name
        );
    }

    let handle = tokio::spawn(async move {
        loop {
            let notification = match listener.recv().await {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("[subscriptions] PgListener error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            };

            let channel = notification.channel();
            let payload_str = notification.payload();

            // Find the entity this channel belongs to
            let entity = match entities.iter().find(|e| e.notify_channel() == channel) {
                Some(e) => e,
                None => continue,
            };

            match serde_json::from_str::<NotifyPayload>(payload_str) {
                Ok(msg) => {
                    let id = msg
                        .id
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| msg.id.to_string().trim_matches('"').to_owned());

                    let data = if msg.op.to_uppercase() == "DELETE" {
                        None
                    } else {
                        msg.row
                    };

                    let event = CdcEvent {
                        op: msg.op.to_uppercase(),
                        id: id.clone(),
                        data,
                    };

                    broadcaster.send(&entity.name, event);
                    tracing::debug!(
                        "[subscriptions] forwarded {} event for entity `{}` id={}",
                        msg.op,
                        entity.name,
                        id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[subscriptions] failed to parse notify payload `{payload_str}`: {e}"
                    );
                }
            }
        }
    });

    Ok(handle)
}

// ── Subscription field builder ────────────────────────────────────────────

/// GraphQL type name for a subscription event: `<Entity>Event`
fn event_type_name(entity: &EntityConfig) -> String {
    format!("{}Event", entity.name)
}

/// Build the `<Entity>Event` Object type and the `watch_<entity>` subscription
/// field for a given entity.
pub fn build_subscription_field(
    entity: &EntityConfig,
    broadcaster: Arc<Broadcaster>,
) -> (Object, SubscriptionField) {
    // ── <Entity>Event type ────────────────────────────────────────────────
    // Fields: op (String!), id (String!), data (<Entity>)
    let event_name = event_type_name(entity);
    let entity_name = entity.name.clone();

    let event_type = Object::new(&event_name)
        .field(Field::new(
            "op",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let val = ctx
                        .parent_value
                        .as_value()
                        .and_then(|v| {
                            if let GqlValue::Object(m) = v {
                                m.get("op").cloned()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(GqlValue::Null);
                    Ok(Some(val))
                })
            },
        ))
        .field(Field::new(
            "id",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let val = ctx
                        .parent_value
                        .as_value()
                        .and_then(|v| {
                            if let GqlValue::Object(m) = v {
                                m.get("id").cloned()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(GqlValue::Null);
                    Ok(Some(val))
                })
            },
        ))
        .field(Field::new(
            "data",
            TypeRef::named(&entity_name), // nullable — None on DELETE
            |ctx| {
                FieldFuture::new(async move {
                    let val = ctx
                        .parent_value
                        .as_value()
                        .and_then(|v| {
                            if let GqlValue::Object(m) = v {
                                m.get("data").cloned()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(GqlValue::Null);
                    Ok(if val == GqlValue::Null {
                        None
                    } else {
                        Some(val)
                    })
                })
            },
        ));

    // ── watch_<entity> subscription field ────────────────────────────────
    let field_name = format!("watch_{}", crate::graphql::snake_pub(&entity.name));
    let event_type_ref = event_type_name(entity);
    let entity_name_sub = entity.name.clone();

    let sub_field = SubscriptionField::new(
        field_name,
        TypeRef::named_nn(&event_type_ref),
        move |_ctx| {
            let broadcaster = Arc::clone(&broadcaster);
            let entity_name = entity_name_sub.clone();
            SubscriptionFieldFuture::new(async move {
                let rx = broadcaster.subscribe(&entity_name).ok_or_else(|| {
                    async_graphql::Error::new(format!(
                        "No subscription channel for entity `{entity_name}`"
                    ))
                })?;

                let raw = BroadcastStream::new(rx);

                // filter_map: async closure, drops lagged-message errors
                let filtered = FuturesStreamExt::filter_map(raw, |result| async move {
                    result.ok() // BroadcastStreamRecvError::Lagged → None → skipped
                });

                // map: convert CdcEvent → Ok(GqlValue)
                let stream = FuturesStreamExt::map(filtered, move |event: CdcEvent| {
                    let data_val = match &event.data {
                        Some(json) => json_to_gql(json.clone()),
                        None => GqlValue::Null,
                    };
                    let mut map = indexmap::IndexMap::new();
                    map.insert(Name::new("op"), GqlValue::String(event.op.clone()));
                    map.insert(Name::new("id"), GqlValue::String(event.id.clone()));
                    map.insert(Name::new("data"), data_val);
                    Ok::<GqlValue, async_graphql::Error>(GqlValue::Object(map))
                });

                Ok(FuturesStreamExt::boxed(stream))
            })
        },
    );

    (event_type, sub_field)
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn json_to_gql(v: serde_json::Value) -> GqlValue {
    match v {
        serde_json::Value::Null => GqlValue::Null,
        serde_json::Value::Bool(b) => GqlValue::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                GqlValue::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                GqlValue::Number(async_graphql::Number::from_f64(f).unwrap_or_else(|| 0i32.into()))
            } else {
                GqlValue::Null
            }
        }
        serde_json::Value::String(s) => GqlValue::String(s),
        serde_json::Value::Array(arr) => GqlValue::List(arr.into_iter().map(json_to_gql).collect()),
        serde_json::Value::Object(obj) => GqlValue::Object(
            obj.into_iter()
                .map(|(k, v)| (Name::new(k), json_to_gql(v)))
                .collect::<indexmap::IndexMap<Name, GqlValue>>(),
        ),
    }
}
