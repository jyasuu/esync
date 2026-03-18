use crate::{config::Config, db, elastic::EsClient};
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

/// Expected shape of the NOTIFY payload (from a trigger).
#[derive(Debug, Deserialize)]
struct NotifyPayload {
    op:  String,           // INSERT | UPDATE | DELETE
    id:  serde_json::Value,
    row: Option<serde_json::Value>,
}

pub async fn run(cfg: Config, args: WatchArgs) -> Result<()> {
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es   = EsClient::new(&cfg.elasticsearch)?;

    let entities: Vec<_> = if args.entity.is_empty() {
        cfg.entities.iter().collect()
    } else {
        cfg.entities.iter()
            .filter(|e| args.entity.contains(&e.name))
            .collect()
    };

    let mut listener = PgListener::connect_with(&pool).await?;
    for entity in &entities {
        let channel = entity.notify_channel();
        listener.listen(channel).await?;
        tracing::info!("Listening on channel `{channel}` → index `{}`", entity.index);
    }

    tracing::info!("CDC watch started. Ctrl-C to stop.");

    loop {
        let notification = listener.recv().await?;
        let channel = notification.channel();
        let payload = notification.payload();

        tracing::debug!("Channel={channel} payload={payload}");

        // Find matching entity
        let entity = entities.iter().find(|e| e.notify_channel() == channel);
        let Some(entity) = entity else { continue; };

        match serde_json::from_str::<NotifyPayload>(payload) {
            Ok(msg) => {
                let id = msg.id.as_str()
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| msg.id.to_string().trim_matches('"').to_owned());

                match msg.op.to_uppercase().as_str() {
                    "INSERT" | "UPDATE" => {
                        if let Some(row) = msg.row {
                            match es.put_document(&entity.index, &id, row).await {
                                Ok(_)  => tracing::info!("[{}] upsert {}", entity.index, id),
                                Err(e) => tracing::error!("ES upsert failed: {e}"),
                            }
                        } else {
                            // Row not in payload — fetch from DB
                            let cols: Vec<&str> = entity.columns.iter()
                                .map(|c| c.name.as_str())
                                .collect();
                            let filter = format!(
                                "{} = '{}'",
                                entity.id_column,
                                id.replace('\'', "''")
                            );
                            let mut rows = crate::db::fetch_rows(
                                &pool, &entity.table, &cols, Some(&filter), 1, 0,
                            ).await?;
                            if let Some(row) = rows.pop() {
                                es.put_document(&entity.index, &id, serde_json::json!(row)).await?;
                                tracing::info!("[{}] upsert (fetched) {}", entity.index, id);
                            }
                        }
                    }
                    "DELETE" => {
                        match es.delete_document(&entity.index, &id).await {
                            Ok(_)  => tracing::info!("[{}] delete {}", entity.index, id),
                            Err(e) => tracing::error!("ES delete failed: {e}"),
                        }
                    }
                    other => tracing::warn!("Unknown op: {other}"),
                }
            }
            Err(e) => {
                tracing::warn!("Failed to parse notify payload `{payload}`: {e}");
            }
        }
    }
}
