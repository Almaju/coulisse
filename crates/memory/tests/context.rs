//! Behavioral tests for context assembly: recall, ordering, budgeting.

use memory::{
    BackendConfig, EmbedderConfig, MemoryConfig, MemoryKind, Role, Store, TokenCount, UserId,
};

async fn new_store() -> Store {
    let config = MemoryConfig {
        backend: BackendConfig::InMemory,
        embedder: EmbedderConfig::Hash { dims: 128 },
        ..MemoryConfig::default()
    };
    Store::open(
        memory::open_pool(&config.backend).await.unwrap(),
        config,
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn recall_ranks_semantically_similar_content_first() {
    let store = new_store().await;
    let user = UserId::new();
    let um = store.for_user(user);

    um.remember(
        MemoryKind::Fact,
        "user loves italian food pasta pizza".into(),
    )
    .await
    .unwrap();
    um.remember(MemoryKind::Fact, "user drives a red tesla car".into())
        .await
        .unwrap();
    um.remember(MemoryKind::Preference, "user enjoys jazz music".into())
        .await
        .unwrap();

    let top = um.recall("pizza pasta italian", 1).await.unwrap();
    assert_eq!(top.len(), 1);
    assert!(
        top[0].content.contains("pasta"),
        "expected pasta memory on top, got {top:?}"
    );
}

#[tokio::test]
async fn assemble_context_returns_messages_chronologically() {
    let store = new_store().await;
    let user = UserId::new();
    let um = store.for_user(user);

    um.append_message(Role::User, "first".into()).await.unwrap();
    um.append_message(Role::Assistant, "second".into())
        .await
        .unwrap();
    um.append_message(Role::User, "third".into()).await.unwrap();

    let ctx = um
        .assemble_context("anything", TokenCount(1_000))
        .await
        .unwrap();
    let contents: Vec<_> = ctx.messages.iter().map(|m| m.content.clone()).collect();
    assert_eq!(contents, vec!["first", "second", "third"]);
}

#[tokio::test]
async fn assemble_context_drops_oldest_when_over_budget() {
    let store = new_store().await;
    let user = UserId::new();
    let um = store.for_user(user);

    // Each message ~= 25 tokens (100 chars / 4). Budget of 60 tokens fits ~2 messages.
    let long = "a".repeat(100);
    for i in 0..5 {
        um.append_message(Role::User, format!("{long}-{i}"))
            .await
            .unwrap();
    }

    let ctx = um.assemble_context("q", TokenCount(60)).await.unwrap();
    // The most recent messages should be kept; oldest dropped.
    assert!(
        ctx.messages.len() < 5,
        "expected some messages to be dropped"
    );
    assert!(!ctx.messages.is_empty(), "expected at least one message");
    let last = ctx.messages.last().unwrap();
    assert!(
        last.content.ends_with("-4"),
        "newest message must be kept, got {}",
        last.content
    );
}

#[tokio::test]
async fn assemble_context_includes_recalled_memories() {
    let store = new_store().await;
    let user = UserId::new();
    let um = store.for_user(user);

    um.remember(MemoryKind::Preference, "prefers dark mode".into())
        .await
        .unwrap();
    um.remember(MemoryKind::Fact, "lives in Paris".into())
        .await
        .unwrap();

    let ctx = um
        .assemble_context("dark mode settings", TokenCount(1_000))
        .await
        .unwrap();
    assert!(!ctx.memories.is_empty());
    assert!(ctx.memories[0].content.contains("dark"));
}
