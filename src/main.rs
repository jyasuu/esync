// Modules live in lib.rs; the binary imports them via the crate name.
use esync::{commands, config};
use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// esync — Sync PostgreSQL → Elasticsearch via GraphQL
#[derive(Parser)]
#[command(
    name = "esync",
    version,
    about = "Sync PostgreSQL data to Elasticsearch via a GraphQL layer",
    long_about = None,
)]
struct Cli {
    /// Path to config file (default: esync.yaml)
    #[arg(short, long, default_value = "esync.yaml", env = "ESYNC_CONFIG")]
    config: String,

    /// Log level: trace | debug | info | warn | error
    #[arg(long, default_value = "info", env = "ESYNC_LOG")]
    log: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the GraphQL server backed by PostgreSQL
    Serve(commands::serve::ServeArgs),

    /// Build (or rebuild) an Elasticsearch index from GraphQL data
    Index(commands::index::IndexArgs),

    /// Elasticsearch index operations
    #[command(subcommand)]
    Es(commands::es::EsCommands),

    /// Watch Postgres LISTEN/NOTIFY for real-time CDC sync
    Watch(commands::watch::WatchArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialise tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(&cli.log))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = config::Config::load(&cli.config)?;

    match cli.command {
        Commands::Serve(args) => commands::serve::run(cfg, args).await,
        Commands::Index(args) => commands::index::run(cfg, args).await,
        Commands::Es(cmd)     => commands::es::run(cfg, cmd).await,
        Commands::Watch(args) => commands::watch::run(cfg, args).await,
    }
}
