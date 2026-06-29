//! FORK-CUSTOM: Integration tests for the fork's `update_env` usage on the
//! agent_metadata repository.
//!
//! Kept in a standalone test file (not inside the upstream `#[cfg(test)] mod
//! tests` of `sqlite_agent_metadata.rs`) so it never collides with upstream
//! test additions during an upstream merge.

use aionui_db::{IAgentMetadataRepository, SqliteAgentMetadataRepository, init_database_memory};

/// Seed row id for "Claude Code" defined in `001_initial_schema.sql`.
const CLAUDE_SEED_ID: &str = "2d23ff1c";

#[tokio::test]
async fn update_env_replaces_column() {
    let db = init_database_memory().await.unwrap();
    let repo = SqliteAgentMetadataRepository::new(db.pool().clone());

    let env_json = r#"[{"name":"ANTHROPIC_BASE_URL","value":"https://api.xaiapi.top"}]"#;
    assert!(repo.update_env(CLAUDE_SEED_ID, env_json).await.unwrap());

    let row = repo.get(CLAUDE_SEED_ID).await.unwrap().unwrap();
    assert_eq!(row.env.as_deref(), Some(env_json));

    // Unknown id should report no row updated.
    assert!(!repo.update_env("missing", env_json).await.unwrap());
}
