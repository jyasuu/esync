/// tests/test_relations.rs
/// Integration tests for GraphQL relationship resolution.
/// Covers: belongs_to, has_many, many_to_many, nullable, and ad-hoc filter.
/// Requires: Postgres (esync.test.yaml with relation entities seeded).
mod common;
use common::*;

use anyhow::Result;
use esync::{config::Config, db};
use serial_test::serial;
use std::time::Duration;
use tokio::task::JoinHandle;

// ── Shared constants ──────────────────────────────────────────────────────

// ── Server setup ──────────────────────────────────────────────────────────

async fn start_server(cfg: Config) -> JoinHandle<()> {
    let args = esync::commands::serve::ServeArgs {
        host: Some("127.0.0.1".into()),
        port: Some(4001),
        no_playground: true,
    };
    let handle = tokio::spawn(async move {
        let _ = esync::commands::serve::run(cfg, args).await;
    });
    let ready = wait_until(
        Duration::from_secs(10),
        Duration::from_millis(100),
        || async {
            reqwest::Client::new()
                .get("http://127.0.0.1:4001/healthz")
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        },
    )
    .await;
    assert!(ready, "Server did not become ready within 10 s");
    handle
}

async fn setup() -> Result<(JoinHandle<()>, sqlx::PgPool)> {
    let cfg = Config::load("esync.test.yaml")?;
    let pool = db::connect(&cfg.postgres.url, cfg.postgres.pool_size).await?;
    reseed(&pool).await?;
    let srv = start_server(cfg).await;
    Ok((srv, pool))
}

// ── belongs_to ────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_belongs_to_resolves_parent() -> Result<()> {
    let (srv, _pool) = setup().await?;

    // ORDER_1 has customer_id = CUSTOMER_ALICE (set by seed)
    let q = format!(
        r#"{{
        get_order(id: "{ORDER_1}") {{
            id
            status
            customer {{
                id
                name
                email
            }}
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let order = &resp["data"]["get_order"];
    assert_eq!(order["id"], ORDER_1);
    assert_eq!(order["customer"]["id"], CUSTOMER_ALICE);
    assert_eq!(order["customer"]["name"], "Alice");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_belongs_to_null_when_no_match() -> Result<()> {
    let (srv, pool) = setup().await?;

    // Set ORDER_2's customer_id to a non-existent UUID
    sqlx::query("UPDATE orders SET customer_id = $1 WHERE id = $2")
        .bind(uuid::Uuid::parse_str(
            "00000000-dead-dead-dead-000000000000",
        )?)
        .bind(uuid::Uuid::parse_str(ORDER_2)?)
        .execute(&pool)
        .await?;

    let q = format!(r#"{{ get_order(id: "{ORDER_2}") {{ id customer {{ id }} }} }}"#);
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);
    assert!(
        resp["data"]["get_order"]["customer"].is_null(),
        "customer should be null when FK target does not exist"
    );

    reseed(&pool).await?;
    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── has_many ──────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_has_many_returns_related_rows() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let q = format!(
        r#"{{
        get_customer(id: "{CUSTOMER_ALICE}") {{
            id
            name
            orders {{
                id
                status
                total
            }}
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let customer = &resp["data"]["get_customer"];
    assert_eq!(customer["name"], "Alice");

    let orders = customer["orders"].as_array().unwrap();
    assert!(!orders.is_empty(), "Alice should have orders");
    // All orders must have customer_id = CUSTOMER_ALICE (enforced by the SQL filter)
    for order in orders {
        assert!(!order["id"].is_null(), "each order must have an id");
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_has_many_empty_for_no_children() -> Result<()> {
    let (srv, _pool) = setup().await?;

    // Bob has no orders
    let q = format!(
        r#"{{
        get_customer(id: "{CUSTOMER_BOB}") {{
            id
            orders {{ id }}
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let orders = resp["data"]["get_customer"]["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 0, "Bob has no orders");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── many_to_many ──────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_many_to_many_returns_related_rows() -> Result<()> {
    let (srv, _pool) = setup().await?;

    let q = format!(
        r#"{{
        get_customer(id: "{CUSTOMER_ALICE}") {{
            id
            tags {{
                id
                label
            }}
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let tags = resp["data"]["get_customer"]["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 2, "Alice has 2 tags (vip + wholesale)");
    let labels: Vec<&str> = tags.iter().filter_map(|t| t["label"].as_str()).collect();
    assert!(labels.contains(&"vip"), "Alice should have 'vip' tag");
    assert!(
        labels.contains(&"wholesale"),
        "Alice should have 'wholesale' tag"
    );

    srv.abort();
    let _ = srv.await;
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_many_to_many_empty_for_no_join_rows() -> Result<()> {
    let (srv, _pool) = setup().await?;

    // Bob has no tags
    let q = format!(
        r#"{{
        get_customer(id: "{CUSTOMER_BOB}") {{
            id
            tags {{ id label }}
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let tags = resp["data"]["get_customer"]["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 0, "Bob has no tags");

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Ad-hoc filter argument ────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_list_with_ad_hoc_filter() -> Result<()> {
    let (srv, _pool) = setup().await?;

    // Use the `filter` argument to scope results
    let q = format!(
        r#"{{
        list_order(filter: "customer_id = '{CUSTOMER_ALICE}'") {{
            id
            customer_id
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let orders = resp["data"]["list_order"].as_array().unwrap();
    assert!(
        !orders.is_empty(),
        "Should find Alice's orders via ad-hoc filter"
    );
    for order in orders {
        assert_eq!(order["customer_id"].as_str().unwrap(), CUSTOMER_ALICE);
    }

    srv.abort();
    let _ = srv.await;
    Ok(())
}

// ── Nested relations ──────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn test_nested_relation_order_customer_tags() -> Result<()> {
    let (srv, _pool) = setup().await?;

    // order → customer → tags (two levels deep)
    let q = format!(
        r#"{{
        get_order(id: "{ORDER_1}") {{
            id
            customer {{
                id
                name
                tags {{
                    label
                }}
            }}
        }}
    }}"#
    );
    let resp = gql(&q, None).await?;
    assert_no_gql_errors(&resp);

    let tags = resp["data"]["get_order"]["customer"]["tags"]
        .as_array()
        .unwrap();
    assert!(!tags.is_empty(), "Should resolve order → customer → tags");

    srv.abort();
    let _ = srv.await;
    Ok(())
}
