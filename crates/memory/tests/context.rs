//! Behavioral tests for context assembly: recall, ordering, budgeting.

use memory::{
    AppendMessage, BackendConfig, ContextRequest, EmbedderConfig, MemoryConfig, MemoryKind,
    RecallQuery, RememberInput, Role, Store, StoreInputs, TokenCount, UserId,
};

async fn new_store() -> Store {
    let config = MemoryConfig {
        backend: BackendConfig::InMemory,
        embedder: EmbedderConfig::Hash { dims: 128 },
        ..MemoryConfig::default()
    };
    Store::open(StoreInputs {
        config: config.clone(),
        fallback_api_key: None,
        pool: memory::open_pool(&config.backend).await.unwrap(),
    })
    .await
    .unwrap()
}

fn fact(content: impl Into<String>) -> RememberInput {
    RememberInput {
        content: content.into(),
        kind: MemoryKind::Fact,
    }
}

fn pref(content: impl Into<String>) -> RememberInput {
    RememberInput {
        content: content.into(),
        kind: MemoryKind::Preference,
    }
}

fn msg(role: Role, content: impl Into<String>) -> AppendMessage {
    AppendMessage {
        content: content.into(),
        id: None,
        role,
    }
}

#[tokio::test]
async fn recall_ranks_semantically_similar_content_first() {
    let store = new_store().await;
    let user = UserId::new();
    let um = store.for_user(user);

    um.remember(fact("user loves italian food pasta pizza"))
        .await
        .unwrap();
    um.remember(fact("user drives a red tesla car"))
        .await
        .unwrap();
    um.remember(pref("user enjoys jazz music")).await.unwrap();

    let top = um
        .recall(RecallQuery {
            k: 1,
            query: "pizza pasta italian",
        })
        .await
        .unwrap();
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

    um.append_message(msg(Role::User, "first")).await.unwrap();
    um.append_message(msg(Role::Assistant, "second"))
        .await
        .unwrap();
    um.append_message(msg(Role::User, "third")).await.unwrap();

    let ctx = um
        .assemble_context(ContextRequest {
            budget: TokenCount(1_000),
            new_user_message: "anything",
        })
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
        um.append_message(msg(Role::User, format!("{long}-{i}")))
            .await
            .unwrap();
    }

    let ctx = um
        .assemble_context(ContextRequest {
            budget: TokenCount(60),
            new_user_message: "q",
        })
        .await
        .unwrap();
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

    um.remember(pref("prefers dark mode")).await.unwrap();
    um.remember(fact("lives in Paris")).await.unwrap();

    let ctx = um
        .assemble_context(ContextRequest {
            budget: TokenCount(1_000),
            new_user_message: "dark mode settings",
        })
        .await
        .unwrap();
    assert!(!ctx.memories.is_empty());
    assert!(ctx.memories[0].content.contains("dark"));
}
