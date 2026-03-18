pub mod mapping;
pub mod search_text;

use crate::{
    config::{Config, EntityConfig},
    db,
    elastic::EsClient,
};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::{json, Value};
use sqlx::PgPool;

/// Full index rebuild for one entity.
/// Injects a `search_text` field into every ES document when configured.
pub async fn rebuild_index(
    pool: &PgPool,
    es: &EsClient,
    entity: &EntityConfig,
    config: &Config,
) -> Result<()> {
    let columns: Vec<&str> = entity.columns.iter().map(|c| c.name.as_str()).collect();

    let total = db::count_rows(pool, &entity.table, entity.filter.as_deref()).await?;
    tracing::info!(
        "Indexing {} rows from `{}` → `{}`",
        total,
        entity.table,
        entity.index
    );

    // Create the ES index — add a `text` mapping for search_text if configured
    let st_field = entity.search_text.as_ref().map(|c| c.field.as_str());
    let body = mapping::build_index_body(&entity.columns, 1, 0, st_field);
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

        let mut docs: Vec<(String, Value)> = Vec::with_capacity(rows.len());

        for mut row in rows {
            // Extract the document id
            let raw_id = match row.remove(&entity.id_column) {
                Some(v) => v,
                None => continue,
            };
            let id = match &raw_id {
                Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_owned(),
            };

            // Build and inject search_text when configured
            if let Some(st_cfg) = &entity.search_text {
                match search_text::build(&row, pool, entity, st_cfg, config).await {
                    Ok(text) => {
                        row.insert(st_cfg.field.clone(), Value::String(text));
                    }
                    Err(e) => {
                        tracing::warn!("search_text build failed for {}:{id} — {e}", entity.name)
                    }
                }
            }

            docs.push((id, json!(row)));
        }

        es.bulk_index(&entity.index, &docs).await?;
        offset += batch;
        pb.inc(docs.len() as u64);
    }

    pb.finish_with_message(format!("✓ {} indexed", entity.index));
    Ok(())
}

/// Rebuild the `search_text` string for a single row by id.
/// Used by the CDC watch loop to refresh one document after a PG change.
pub async fn build_search_text_for_id(
    pool: &PgPool,
    entity: &EntityConfig,
    id: &str,
    config: &Config,
) -> Result<Option<String>> {
    let cfg = match &entity.search_text {
        Some(c) => c,
        None => return Ok(None),
    };

    let cols: Vec<&str> = entity.columns.iter().map(|c| c.name.as_str()).collect();
    let filter = format!("{} = '{}'", entity.id_column, id.replace('\'', "''"));
    let mut rows = db::fetch_rows(pool, &entity.table, &cols, Some(&filter), 1, 0).await?;
    let row = match rows.pop() {
        Some(r) => r,
        None => return Ok(None),
    };

    let text = search_text::build(&row, pool, entity, cfg, config).await?;
    Ok(Some(text))
}
