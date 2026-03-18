use crate::{config::Config, db, elastic::EsClient, indexer};
use anyhow::Result;
use clap::Args;
use serde::Deserialize;
use sqlx::postgres::PgListener;

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Entity name(s) to watch (defaults to all)
    #[arg(short, long, num_args = 1..)]
    pub entity: Vec<String>,
}

/// Expected JSON shape sent by the Postgres CDC trigger.
#[derive(Debug, Deserialize)]
struct NotifyPayload {
    op: String, // INSERT | UPDATE | DELETE
    id: serde_json::Value,
    row: Option<serde_json::Value>, // full row (from row_to_json) — may be absent
}

pub async fn run(cfg: Config, args: WatchArgs) -> Result<()> {
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es = EsClient::new(&cfg.elasticsearch)?;

    let entities: Vec<_> = if args.entity.is_empty() {
        cfg.entities.iter().collect()
    } else {
        cfg.entities
            .iter()
            .filter(|e| args.entity.contains(&e.name))
            .collect()
    };

    let mut listener = PgListener::connect_with(&pool).await?;
    for entity in &entities {
        let channel = entity.notify_channel();
        listener.listen(channel).await?;
        tracing::info!(
            "Listening on channel `{channel}` → index `{}`",
            entity.index
        );
    }
    tracing::info!("CDC watch started. Ctrl-C to stop.");

    loop {
        let notification = listener.recv().await?;
        let channel = notification.channel();
        let payload = notification.payload();
        tracing::debug!("channel={channel} payload={payload}");

        let entity = match entities.iter().find(|e| e.notify_channel() == channel) {
            Some(e) => *e,
            None => continue,
        };

        match serde_json::from_str::<NotifyPayload>(payload) {
            Ok(msg) => {
                let id = msg
                    .id
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| msg.id.to_string().trim_matches('"').to_owned());

                match msg.op.to_uppercase().as_str() {
                    "INSERT" | "UPDATE" => {
                        // Build the base document from the NOTIFY payload row,
                        // or fall back to a fresh PG fetch if row is absent.
                        let mut doc = match msg.row {
                            Some(r) => r,
                            None => {
                                let cols: Vec<&str> =
                                    entity.columns.iter().map(|c| c.name.as_str()).collect();
                                let filter =
                                    format!("{} = '{}'", entity.id_column, id.replace('\'', "''"));
                                let mut rows = db::fetch_rows(
                                    &pool,
                                    &entity.table,
                                    &cols,
                                    Some(&filter),
                                    1,
                                    0,
                                )
                                .await?;
                                match rows.pop() {
                                    Some(row) => serde_json::json!(row),
                                    None => {
                                        tracing::warn!(
                                            "[{}] row {} not found after notify",
                                            entity.index,
                                            id
                                        );
                                        continue;
                                    }
                                }
                            }
                        };

                        // Rebuild search_text from PG (always fresh — relations
                        // may have changed independently of this row's own columns)
                        if entity.search_text.is_some() {
                            match indexer::build_search_text_for_id(&pool, entity, &id, &cfg).await
                            {
                                Ok(Some(text)) => {
                                    let field = entity
                                        .search_text
                                        .as_ref()
                                        .map(|c| c.field.as_str())
                                        .unwrap_or("search_text");
                                    doc[field] = serde_json::Value::String(text);
                                    tracing::debug!(
                                        "[{}] search_text rebuilt for {id}",
                                        entity.index
                                    );
                                }
                                Ok(None) => {}
                                Err(e) => tracing::error!(
                                    "[{}] search_text build failed for {id}: {e}",
                                    entity.index
                                ),
                            }
                        }

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
            Err(e) => {
                tracing::warn!("failed to parse notify payload `{payload}`: {e}");
            }
        }
    }
}
