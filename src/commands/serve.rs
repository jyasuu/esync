use crate::graphql::subscriptions::{spawn_cdc_listener, Broadcaster};
use crate::{config::Config, db, elastic::EsClient, graphql};
use anyhow::Result;
use async_graphql::http::{GraphiQLSource, ALL_WEBSOCKET_PROTOCOLS};
use async_graphql_axum::{GraphQLProtocol, GraphQLRequest, GraphQLResponse, GraphQLWebSocket};
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
    schema: Arc<DynSchema>,
    playground: bool,
}

pub async fn run(cfg: Config, args: ServeArgs) -> Result<()> {
    let pool = Arc::new(db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?);

    // Connect to ES if any entity needs it
    let needs_es = cfg
        .entities
        .iter()
        .any(|e| e.search.enabled || !e.is_readonly());
    let es = if needs_es {
        Arc::new(EsClient::new(&cfg.elasticsearch)?)
    } else {
        Arc::new(EsClient::new_noop())
    };

    // Build the subscription broadcaster — one channel per writable entity
    let broadcaster = Arc::new(Broadcaster::new(&cfg));

    // Spin up an embedded CDC listener so subscriptions work without a
    // separate `esync watch` process.
    let cfg_arc = Arc::new(cfg.clone());
    let _cdc_handle = spawn_cdc_listener(
        Arc::clone(&cfg_arc),
        Arc::clone(&pool),
        Arc::clone(&broadcaster),
    )
    .await?;

    let schema = Arc::new(graphql::build_schema(
        &cfg,
        Arc::clone(&pool),
        Arc::clone(&es),
        Arc::clone(&broadcaster),
    )?);

    let host = args
        .host
        .as_deref()
        .unwrap_or(&cfg.graphql.host)
        .to_string();
    let port = args.port.unwrap_or(cfg.graphql.port);
    let playground = cfg.graphql.playground && !args.no_playground;

    let state = AppState { schema, playground };

    let app = Router::new()
        .route("/graphql", post(graphql_handler))
        .route("/graphql/ws", get(graphql_ws_handler))
        .route("/graphql", get(playground_handler))
        .route("/healthz", get(health_handler))
        .with_state(state)
        .layer(tower_http::cors::CorsLayer::permissive());

    let addr = format!("{host}:{port}");
    tracing::info!("GraphQL endpoint         → http://{addr}/graphql");
    tracing::info!("GraphQL subscriptions WS → ws://{addr}/graphql/ws");
    if playground {
        tracing::info!("GraphiQL playground      → http://{addr}/graphql");
    }

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn graphql_handler(State(state): State<AppState>, req: GraphQLRequest) -> GraphQLResponse {
    state.schema.execute(req.into_inner()).await.into()
}

async fn graphql_ws_handler(
    State(state): State<AppState>,
    protocol: GraphQLProtocol,
    websocket: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    let schema = Arc::clone(&state.schema);
    websocket
        .protocols(ALL_WEBSOCKET_PROTOCOLS)
        .on_upgrade(move |stream| {
            GraphQLWebSocket::new(stream, schema.as_ref().clone(), protocol).serve()
        })
}

async fn playground_handler(State(state): State<AppState>) -> impl IntoResponse {
    if state.playground {
        Html(
            GraphiQLSource::build()
                .endpoint("/graphql")
                .subscription_endpoint("/graphql/ws")
                .finish(),
        )
        .into_response()
    } else {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}

async fn health_handler() -> &'static str {
    "ok"
}
