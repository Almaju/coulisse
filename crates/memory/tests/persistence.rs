//! Verifies that a file-backed Store retains data across restarts and that
//! the dedup path skips near-duplicates.

use memory::{BackendConfig, EmbedderConfig, MemoryConfig, MemoryKind, Role, Store, UserId};

fn config_at(path: std::path::PathBuf) -> MemoryConfig {
    MemoryConfig {
        backend: BackendConfig::Sqlite { path },
        embedder: EmbedderConfig::Hash { dims: 64 },
        ..MemoryConfig::default()
    }
}

#[tokio::test]
async fn data_survives_process_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("coulisse.db");
    let user = UserId::new();

    let store = Store::open(
        memory::open_pool(&BackendConfig::Sqlite { path: db.clone() })
            .await
            .unwrap(),
        config_at(db.clone()),
        None,
    )
    .await
    .unwrap();
    store
        .for_user(user)
        .remember(MemoryKind::Fact, "user lives in Paris".into())
        .await
        .unwrap();
    store
        .for_user(user)
        .append_message(Role::User, "hello".into())
        .await
        .unwrap();
    drop(store);

    let reopened = Store::open(
        memory::open_pool(&BackendConfig::Sqlite { path: db.clone() })
            .await
            .unwrap(),
        config_at(db),
        None,
    )
    .await
    .unwrap();
    let memories = reopened.for_user(user).memories().await.unwrap();
    let messages = reopened.for_user(user).messages().await.unwrap();
    assert_eq!(memories.len(), 1);
    assert!(memories[0].content.contains("Paris"));
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "hello");
}

#[tokio::test]
async fn remember_if_novel_skips_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("coulisse.db");
    let user = UserId::new();

    let store = Store::open(
        memory::open_pool(&BackendConfig::Sqlite { path: db.clone() })
            .await
            .unwrap(),
        config_at(db),
        None,
    )
    .await
    .unwrap();
    let um = store.for_user(user);

    let first = um
        .remember_if_novel(MemoryKind::Fact, "user lives in Paris".into(), 0.9)
        .await
        .unwrap();
    assert!(first.is_some());

    let duplicate = um
        .remember_if_novel(MemoryKind::Fact, "user lives in Paris".into(), 0.9)
        .await
        .unwrap();
    assert!(
        duplicate.is_none(),
        "exact duplicate should have been skipped"
    );

    let different = um
        .remember_if_novel(MemoryKind::Fact, "user drives a tesla".into(), 0.9)
        .await
        .unwrap();
    assert!(different.is_some(), "unrelated fact should be stored");

    assert_eq!(um.memory_count().await.unwrap(), 2);
}

#[tokio::test]
async fn recall_ignores_memories_from_different_embedder_model() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("coulisse.db");
    let user = UserId::new();

    let cfg_a = MemoryConfig {
        backend: BackendConfig::Sqlite { path: db.clone() },
        embedder: EmbedderConfig::Hash { dims: 32 },
        ..MemoryConfig::default()
    };
    let pool_a = memory::open_pool(&cfg_a.backend).await.unwrap();
    let store_a = Store::open(pool_a, cfg_a, None).await.unwrap();
    store_a
        .for_user(user)
        .remember(MemoryKind::Fact, "written by embedder A".into())
        .await
        .unwrap();
    drop(store_a);

    // Reopen with a different embedder (same dims doesn't matter — the
    // model_id differs because the dims do).
    let cfg_b = MemoryConfig {
        backend: BackendConfig::Sqlite { path: db.clone() },
        embedder: EmbedderConfig::Hash { dims: 64 },
        ..MemoryConfig::default()
    };
    let pool_b = memory::open_pool(&cfg_b.backend).await.unwrap();
    let store_b = Store::open(pool_b, cfg_b, None).await.unwrap();

    let recalled = store_b.for_user(user).recall("anything", 10).await.unwrap();
    assert!(
        recalled.is_empty(),
        "memories from a different embedder should not be recalled, got {recalled:?}"
    );
}
