//! Verifies that `UserMemory` handles cannot observe or mutate data belonging
//! to other users. If any assertion here fails, user isolation has broken.

use memory::testing::HashEmbedder;
use memory::{MemoryConfig, MemoryKind, Role, Store, TokenCount, UserId};

fn new_store() -> Store<HashEmbedder> {
    Store::new(HashEmbedder::new(64), MemoryConfig::default())
}

#[tokio::test]
async fn memories_are_not_visible_across_users() {
    let store = new_store();
    let alice = UserId::new();
    let bob = UserId::new();

    store
        .for_user(alice)
        .await
        .remember(MemoryKind::Fact, "alice likes pizza".into())
        .await
        .unwrap();
    store
        .for_user(bob)
        .await
        .remember(MemoryKind::Fact, "bob hates pineapple".into())
        .await
        .unwrap();

    let alice_recall = store
        .for_user(alice)
        .await
        .recall("food", 10)
        .await
        .unwrap();
    let bob_recall = store.for_user(bob).await.recall("food", 10).await.unwrap();

    assert_eq!(alice_recall.len(), 1);
    assert_eq!(bob_recall.len(), 1);
    assert!(alice_recall[0].content.contains("alice"));
    assert!(bob_recall[0].content.contains("bob"));
    assert_eq!(alice_recall[0].user_id, alice);
    assert_eq!(bob_recall[0].user_id, bob);
}

#[tokio::test]
async fn messages_are_not_visible_across_users() {
    let store = new_store();
    let alice = UserId::new();
    let bob = UserId::new();

    store
        .for_user(alice)
        .await
        .append_message(Role::User, "alice says hi".into())
        .await
        .unwrap();
    store
        .for_user(bob)
        .await
        .append_message(Role::User, "bob says hello".into())
        .await
        .unwrap();

    let alice_ctx = store
        .for_user(alice)
        .await
        .assemble_context("hi", TokenCount(1_000))
        .await
        .unwrap();
    let bob_ctx = store
        .for_user(bob)
        .await
        .assemble_context("hi", TokenCount(1_000))
        .await
        .unwrap();

    assert_eq!(alice_ctx.messages.len(), 1);
    assert_eq!(bob_ctx.messages.len(), 1);
    assert!(alice_ctx.messages[0].content.contains("alice"));
    assert!(bob_ctx.messages[0].content.contains("bob"));
}

#[tokio::test]
async fn empty_user_sees_empty_context() {
    let store = new_store();
    let ghost = UserId::new();

    let ctx = store
        .for_user(ghost)
        .await
        .assemble_context("anything", TokenCount(1_000))
        .await
        .unwrap();

    assert!(ctx.memories.is_empty());
    assert!(ctx.messages.is_empty());
}

#[tokio::test]
async fn counts_are_per_user() {
    let store = new_store();
    let alice = UserId::new();
    let bob = UserId::new();

    for i in 0..5 {
        store
            .for_user(alice)
            .await
            .append_message(Role::User, format!("alice msg {i}"))
            .await
            .unwrap();
    }
    for i in 0..2 {
        store
            .for_user(bob)
            .await
            .append_message(Role::User, format!("bob msg {i}"))
            .await
            .unwrap();
    }

    assert_eq!(store.for_user(alice).await.message_count().await, 5);
    assert_eq!(store.for_user(bob).await.message_count().await, 2);
}
