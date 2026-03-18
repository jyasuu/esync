use crate::{config::Config, db, elastic::EsClient, graphql};
use anyhow::Result;
use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::{get, post},
    Router,
};
use clap::Args;
use std::sync::Arc;

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Override listen host
    #[arg(long)]
    pub host: Option<String>,
    /// Override listen port
    #[arg(long, short)]
    pub port: Option<u16>,
    /// Disable GraphiQL playground
    #[arg(long)]
    pub no_playground: bool,
}

type DynSchema = async_graphql::dynamic::Schema;

#[derive(Clone)]
struct AppState {
    schema:     Arc<DynSchema>,
    playground: bool,
}

pub async fn run(cfg: Config, args: ServeArgs) -> Result<()> {
    let pool = Arc::new(db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?);
    // Only connect to ES if at least one entity has search.enabled
    let es = if cfg.entities.iter().any(|e| e.search.enabled) {
        Arc::new(EsClient::new(&cfg.elasticsearch)?)
    } else {
        Arc::new(EsClient::new_noop())
    };
    let schema = Arc::new(graphql::build_schema(&cfg, pool, es)?);

    let host       = args.host.as_deref().unwrap_or(&cfg.graphql.host).to_string();
    let port       = args.port.unwrap_or(cfg.graphql.port);
    let playground = cfg.graphql.playground && !args.no_playground;

    let state = AppState { schema, playground };

    let app = Router::new()
        .route("/graphql", post(graphql_handler))
        .route("/graphql", get(playground_handler))
        .route("/healthz",  get(health_handler))
        .with_state(state)
        .layer(tower_http::cors::CorsLayer::permissive());

    let addr = format!("{host}:{port}");
    tracing::info!("GraphQL endpoint    → http://{addr}/graphql");
    if playground {
        tracing::info!("GraphiQL playground → http://{addr}/graphql");
    }

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn graphql_handler(
    State(state): State<AppState>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    state.schema.execute(req.into_inner()).await.into()
}

async fn playground_handler(State(state): State<AppState>) -> impl IntoResponse {
    if state.playground {
        Html(
            GraphiQLSource::build()
                .endpoint("/graphql")
                .finish(),
        ).into_response()
    } else {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}

async fn health_handler() -> &'static str {
    "ok"
}
