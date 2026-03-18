// lib.rs — exposes all modules for integration tests.
// The binary (main.rs) is the actual entry point; this just re-exports
// everything so `use esync::…` works in tests/*.rs.

pub mod commands;
pub mod config;
pub mod db;
pub mod elastic;
pub mod graphql;
pub mod indexer;
