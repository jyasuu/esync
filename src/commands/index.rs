use crate::{config::Config, db, elastic::EsClient, indexer};
use anyhow::{bail, Result};
use clap::Args;

#[derive(Args, Debug)]
pub struct IndexArgs {
    /// Entity name(s) to index (defaults to all)
    #[arg(short, long, num_args = 1..)]
    pub entity: Vec<String>,

    /// Recreate index even if it already exists
    #[arg(long)]
    pub recreate: bool,

    /// Number of shards for new indices
    #[arg(long, default_value = "1")]
    pub shards: u32,

    /// Number of replicas for new indices
    #[arg(long, default_value = "0")]
    pub replicas: u32,
}

pub async fn run(cfg: Config, args: IndexArgs) -> Result<()> {
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    let es   = EsClient::new(&cfg.elasticsearch)?;

    let entities: Vec<_> = if args.entity.is_empty() {
        cfg.entities.iter().collect()
    } else {
        let filtered: Vec<_> = cfg.entities.iter()
            .filter(|e| args.entity.contains(&e.name))
            .collect();
        if filtered.is_empty() {
            bail!("No matching entities found for {:?}", args.entity);
        }
        filtered
    };

    for entity in entities {
        indexer::rebuild_index(&pool, &es, entity).await?;
    }

    tracing::info!("All done ✓");
    Ok(())
}
