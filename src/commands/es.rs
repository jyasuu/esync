use crate::{config::Config, elastic::EsClient};
use anyhow::Result;
use clap::{Args, Subcommand};
use colored::Colorize;
use std::fs;

pub async fn run(cfg: Config, cmd: EsCommands) -> Result<()> {
    let es = EsClient::new(&cfg.elasticsearch)?;
    match cmd {
        EsCommands::Index(c) => index_cmd(es, c).await,
        EsCommands::Doc(c) => doc_cmd(es, c).await,
        EsCommands::Search(c) => search_cmd(es, c).await,
        EsCommands::Datastream(c) => datastream_cmd(es, c).await,
        EsCommands::Template(c) => template_cmd(es, c).await,
        EsCommands::Policy(c) => policy_cmd(es, c).await,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Sub-command tree
// ────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum EsCommands {
    /// Index management (create, delete, list, mappings, …)
    #[command(subcommand)]
    Index(IndexCommands),

    /// Document operations (get, put, delete)
    #[command(subcommand)]
    Doc(DocCommands),

    /// Run a search query from a JSON file
    Search(SearchArgs),

    /// Data stream management
    #[command(subcommand)]
    Datastream(DatastreamCommands),

    /// Index template management
    #[command(subcommand)]
    Template(TemplateCommands),

    /// ILM policy management
    #[command(subcommand)]
    Policy(PolicyCommands),
}

// ── Index ────────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum IndexCommands {
    /// List indices (supports wildcard pattern)
    List(PatternArg),
    /// Show index details + mappings
    Get(NameArg),
    /// Create index from JSON body file
    Create(BodyArg),
    /// Delete index
    Delete(NameArg),
    /// Show current mappings
    Mappings(NameArg),
    /// Update mappings from JSON file
    PutMappings(BodyArg),
}

async fn index_cmd(es: EsClient, cmd: IndexCommands) -> Result<()> {
    match cmd {
        IndexCommands::List(a) => print_json(es.list_indices(&a.pattern).await?),
        IndexCommands::Get(a) => print_json(es.get_index(&a.name).await?),
        IndexCommands::Create(a) => {
            let body = read_json(&a.file)?;
            print_json(es.create_index(&a.name, body).await?);
        }
        IndexCommands::Delete(a) => print_json(es.delete_index(&a.name).await?),
        IndexCommands::Mappings(a) => print_json(es.index_mappings(&a.name).await?),
        IndexCommands::PutMappings(a) => {
            let body = read_json(&a.file)?;
            print_json(es.put_mapping(&a.name, body).await?);
        }
    }
    Ok(())
}

// ── Documents ────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum DocCommands {
    /// Get a document by id
    Get(DocGetArgs),
    /// Index (upsert) a document from JSON file
    Put(DocPutArgs),
    /// Delete a document by id
    Delete(DocGetArgs),
}

async fn doc_cmd(es: EsClient, cmd: DocCommands) -> Result<()> {
    match cmd {
        DocCommands::Get(a) => print_json(es.get_document(&a.index, &a.id).await?),
        DocCommands::Delete(a) => print_json(es.delete_document(&a.index, &a.id).await?),
        DocCommands::Put(a) => {
            let body = read_json(&a.file)?;
            print_json(es.put_document(&a.index, &a.id, body).await?);
        }
    }
    Ok(())
}

// ── Search ───────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Index to search
    #[arg(short, long)]
    pub index: String,
    /// Path to JSON query body (Elasticsearch DSL)
    #[arg(short, long)]
    pub file: String,
}

async fn search_cmd(es: EsClient, args: SearchArgs) -> Result<()> {
    let body = read_json(&args.file)?;
    print_json(es.search(&args.index, body).await?);
    Ok(())
}

// ── Datastream ───────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum DatastreamCommands {
    List(PatternArg),
    Create(NameArg),
    Delete(NameArg),
}

async fn datastream_cmd(es: EsClient, cmd: DatastreamCommands) -> Result<()> {
    match cmd {
        DatastreamCommands::List(a) => print_json(es.list_datastreams(&a.pattern).await?),
        DatastreamCommands::Create(a) => print_json(es.create_datastream(&a.name).await?),
        DatastreamCommands::Delete(a) => print_json(es.delete_datastream(&a.name).await?),
    }
    Ok(())
}

// ── Template ─────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum TemplateCommands {
    Get(NameArg),
    /// Create/update template from JSON file
    Put(BodyArg),
    Delete(NameArg),
}

async fn template_cmd(es: EsClient, cmd: TemplateCommands) -> Result<()> {
    match cmd {
        TemplateCommands::Get(a) => print_json(es.get_template(&a.name).await?),
        TemplateCommands::Delete(a) => print_json(es.delete_template(&a.name).await?),
        TemplateCommands::Put(a) => {
            let body = read_json(&a.file)?;
            print_json(es.put_template(&a.name, body).await?);
        }
    }
    Ok(())
}

// ── Policy ───────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum PolicyCommands {
    Get(NameArg),
    /// Create/update ILM policy from JSON file
    Put(BodyArg),
    Delete(NameArg),
}

async fn policy_cmd(es: EsClient, cmd: PolicyCommands) -> Result<()> {
    match cmd {
        PolicyCommands::Get(a) => print_json(es.get_policy(&a.name).await?),
        PolicyCommands::Delete(a) => print_json(es.delete_policy(&a.name).await?),
        PolicyCommands::Put(a) => {
            let body = read_json(&a.file)?;
            print_json(es.put_policy(&a.name, body).await?);
        }
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Shared arg structs
// ────────────────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct NameArg {
    pub name: String,
}

#[derive(Args, Debug)]
pub struct PatternArg {
    #[arg(default_value = "*")]
    pub pattern: String,
}

#[derive(Args, Debug)]
pub struct BodyArg {
    pub name: String,
    /// Path to JSON body file
    #[arg(short, long)]
    pub file: String,
}

#[derive(Args, Debug)]
pub struct DocGetArgs {
    pub index: String,
    pub id: String,
}

#[derive(Args, Debug)]
pub struct DocPutArgs {
    pub index: String,
    pub id: String,
    #[arg(short, long)]
    pub file: String,
}

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

fn read_json(path: &str) -> Result<serde_json::Value> {
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn print_json(v: serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&v).unwrap_or_default().green()
    );
}
