//! Verifies that `UserMemory` handles cannot observe or mutate data belonging
//! to other users. If any assertion here fails, user isolation has broken.

use memory::{
    AppendMessage, BackendConfig, ContextRequest, EmbedderConfig, MemoryConfig, MemoryKind,
    RecallQuery, RememberInput, Role, Store, StoreInputs, TokenCount, UserId,
};

async fn new_store() -> Store {
    let config = MemoryConfig {
        backend: BackendConfig::InMemory,
        embedder: EmbedderConfig::Hash { dims: 64 },
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

#[tokio::test]
async fn memories_are_not_visible_across_users() {
    let store = new_store().await;
    let alice = UserId::new();
    let bob = UserId::new();

    store
        .for_user(alice)
        .remember(RememberInput {
            content: "alice likes pizza".into(),
            kind: MemoryKind::Fact,
        })
        .await
        .unwrap();
    store
        .for_user(bob)
        .remember(RememberInput {
            content: "bob hates pineapple".into(),
            kind: MemoryKind::Fact,
        })
        .await
        .unwrap();

    let alice_recall = store
        .for_user(alice)
        .recall(RecallQuery {
            k: 10,
            query: "food",
        })
        .await
        .unwrap();
    let bob_recall = store
        .for_user(bob)
        .recall(RecallQuery {
            k: 10,
            query: "food",
        })
        .await
        .unwrap();

    assert_eq!(alice_recall.len(), 1);
    assert_eq!(bob_recall.len(), 1);
    assert!(alice_recall[0].content.contains("alice"));
    assert!(bob_recall[0].content.contains("bob"));
    assert_eq!(alice_recall[0].user_id, alice);
    assert_eq!(bob_recall[0].user_id, bob);
}

#[tokio::test]
async fn messages_are_not_visible_across_users() {
    let store = new_store().await;
    let alice = UserId::new();
    let bob = UserId::new();

    store
        .for_user(alice)
        .append_message(AppendMessage {
            content: "alice says hi".into(),
            id: None,
            role: Role::User,
        })
        .await
        .unwrap();
    store
        .for_user(bob)
        .append_message(AppendMessage {
            content: "bob says hello".into(),
            id: None,
            role: Role::User,
        })
        .await
        .unwrap();

    let alice_ctx = store
        .for_user(alice)
        .assemble_context(ContextRequest {
            budget: TokenCount(1_000),
            new_user_message: "hi",
        })
        .await
        .unwrap();
    let bob_ctx = store
        .for_user(bob)
        .assemble_context(ContextRequest {
            budget: TokenCount(1_000),
            new_user_message: "hi",
        })
        .await
        .unwrap();

    assert_eq!(alice_ctx.messages.len(), 1);
    assert_eq!(bob_ctx.messages.len(), 1);
    assert!(alice_ctx.messages[0].content.contains("alice"));
    assert!(bob_ctx.messages[0].content.contains("bob"));
}

#[tokio::test]
async fn empty_user_sees_empty_context() {
    let store = new_store().await;
    let ghost = UserId::new();

    let ctx = store
        .for_user(ghost)
        .assemble_context(ContextRequest {
            budget: TokenCount(1_000),
            new_user_message: "anything",
        })
        .await
        .unwrap();

    assert!(ctx.memories.is_empty());
    assert!(ctx.messages.is_empty());
}

#[tokio::test]
async fn counts_are_per_user() {
    let store = new_store().await;
    let alice = UserId::new();
    let bob = UserId::new();

    for i in 0..5 {
        store
            .for_user(alice)
            .append_message(AppendMessage {
                content: format!("alice msg {i}"),
                id: None,
                role: Role::User,
            })
            .await
            .unwrap();
    }
    for i in 0..2 {
        store
            .for_user(bob)
            .append_message(AppendMessage {
                content: format!("bob msg {i}"),
                id: None,
                role: Role::User,
            })
            .await
            .unwrap();
    }

    assert_eq!(store.for_user(alice).message_count().await.unwrap(), 5);
    assert_eq!(store.for_user(bob).message_count().await.unwrap(), 2);
}
