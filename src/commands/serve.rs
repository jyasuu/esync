use crate::auth::{AuthContext, ExtractAuth, TokenValidator};
use crate::graphql::subscriptions::{spawn_cdc_listener, Broadcaster};
use crate::{config::Config, db, elastic::EsClient, graphql};
use anyhow::Result;
use async_graphql::http::{GraphiQLSource, ALL_WEBSOCKET_PROTOCOLS};
use async_graphql_axum::{GraphQLProtocol, GraphQLRequest, GraphQLResponse, GraphQLWebSocket};
use axum::http::Request;
use axum::{
    extract::State,
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use clap::Args;
use std::sync::Arc;

#[derive(Args, Debug)]
pub struct ServeArgs {
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long, short)]
    pub port: Option<u16>,
    #[arg(long)]
    pub no_playground: bool,
}

type DynSchema = async_graphql::dynamic::Schema;

#[derive(Clone)]
struct AppState {
    schema: Arc<DynSchema>,
    playground: bool,
    validator: Option<Arc<TokenValidator>>,
}

pub async fn run(cfg: Config, args: ServeArgs) -> Result<()> {
    let pool = Arc::new(db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?);

    let needs_es = cfg
        .entities
        .iter()
        .any(|e| e.search.enabled || !e.is_readonly());
    let es = if needs_es {
        Arc::new(EsClient::new(&cfg.elasticsearch)?)
    } else {
        Arc::new(EsClient::new_noop())
    };

    let broadcaster = Arc::new(Broadcaster::new(&cfg));
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

    let validator: Option<Arc<TokenValidator>> = cfg.graphql.oauth2.as_ref().map(|oauth2_cfg| {
        tracing::info!(
            mode = ?oauth2_cfg.validation_mode,
            require_auth = oauth2_cfg.require_auth,
            "OAuth2 authentication enabled"
        );
        Arc::new(TokenValidator::new(Arc::new(oauth2_cfg.clone())))
    });

    let host = args
        .host
        .as_deref()
        .unwrap_or(&cfg.graphql.host)
        .to_string();
    let port = args.port.unwrap_or(cfg.graphql.port);
    let playground = cfg.graphql.playground && !args.no_playground;

    let state = AppState {
        schema,
        playground,
        validator: validator.clone(),
    };

    let app = Router::new()
        .route("/graphql", post(graphql_handler))
        .route("/graphql/ws", get(graphql_ws_handler))
        .route("/graphql", get(playground_handler))
        .route("/healthz", get(health_handler))
        .layer(middleware::from_fn_with_state(
            validator.clone(),
            inject_validator_middleware,
        ))
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

async fn inject_validator_middleware(
    State(validator): State<Option<Arc<TokenValidator>>>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(v) = validator {
        req.extensions_mut().insert(v);
    }
    next.run(req).await
}

async fn graphql_handler(
    State(state): State<AppState>,
    ExtractAuth(auth_ctx): ExtractAuth,
    req: GraphQLRequest,
) -> Result<GraphQLResponse, (StatusCode, String)> {
    let mut inner = req.into_inner();
    inner = inner.data(auth_ctx);
    Ok(state.schema.execute(inner).await.into())
}

async fn graphql_ws_handler(
    State(state): State<AppState>,
    protocol: GraphQLProtocol,
    websocket: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    let schema = Arc::clone(&state.schema);
    let validator = state.validator.clone();

    websocket
        .protocols(ALL_WEBSOCKET_PROTOCOLS)
        .on_upgrade(move |stream| async move {
            GraphQLWebSocket::new(stream, schema.as_ref().clone(), protocol)
                .on_connection_init(move |params| {
                    let validator = validator.clone();
                    async move {
                        let token = params
                            .get("Authorization")
                            .or_else(|| params.get("authorization"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim_start_matches("Bearer ")
                            .trim()
                            .to_owned();

                        let auth_ctx = if token.is_empty() || validator.is_none() {
                            AuthContext::anonymous()
                        } else {
                            let v = validator.unwrap();
                            match v.validate(&token).await {
                                Ok(ctx) => ctx,
                                Err(e) => {
                                    tracing::warn!("WS token validation failed: {e}");
                                    return Err(async_graphql::Error::new(format!(
                                        "Unauthorized: {e}"
                                    )));
                                }
                            }
                        };

                        let mut data = async_graphql::Data::default();
                        data.insert(auth_ctx);
                        Ok(data)
                    }
                })
                .serve()
                .await
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
