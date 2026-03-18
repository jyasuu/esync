pub mod mapping;

use crate::{config::EntityConfig, db, elastic::EsClient};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::{json, Value};
use sqlx::PgPool;

/// Full index rebuild for one entity.
pub async fn rebuild_index(pool: &PgPool, es: &EsClient, entity: &EntityConfig) -> Result<()> {
    let columns: Vec<&str> = entity.columns.iter().map(|c| c.name.as_str()).collect();

    // Count total rows
    let total = db::count_rows(pool, &entity.table, entity.filter.as_deref()).await?;
    tracing::info!(
        "Indexing {} rows from `{}` → `{}`",
        total,
        entity.table,
        entity.index
    );

    // (Re)create index
    let body = mapping::build_index_body(&entity.columns, 1, 0);
    es.recreate_index(&entity.index, body).await?;

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("█▇▆▅▄▃▂▁  "),
    );

    let mut offset: i64 = 0;
    let batch = entity.batch_size as i64;

    loop {
        let rows = db::fetch_rows(
            pool,
            &entity.table,
            &columns,
            entity.filter.as_deref(),
            batch,
            offset,
        )
        .await?;

        if rows.is_empty() {
            break;
        }

        let docs: Vec<(String, Value)> = rows
            .into_iter()
            .filter_map(|mut row| {
                let id = row.remove(&entity.id_column)?.to_string();
                let id = id.trim_matches('"').to_string();
                Some((id, json!(row)))
            })
            .collect();

        es.bulk_index(&entity.index, &docs).await?;
        offset += batch;
        pb.inc(docs.len() as u64);
    }

    pb.finish_with_message(format!("✓ {} indexed", entity.index));
    Ok(())
}
