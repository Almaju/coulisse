//! HTTP-level integration tests for the chat completions endpoint. Drives
//! the real axum router through `tower::ServiceExt::oneshot` against a
//! `ScriptedPrompter` — no network, no real provider. The streaming tests
//! also exercise the `MemoryFlush` Drop guard by dropping the response body
//! mid-stream.
//!
//! Tests use the `current_thread` flavor so a 20ms sleep after dropping a
//! response is enough for the spawned memory-flush task to complete.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use limits::Tracker;
use memory::{BackendConfig, EmbedderConfig, MemoryConfig, Role as MemRole, Store, UserId};
use prompter::testing::{ScriptedPrompter, ScriptedReply};
use prompter::{AgentConfig, ProviderKind, Usage};
use server::{AppState, Server};
use tower::ServiceExt;

fn make_agents() -> Vec<AgentConfig> {
    vec![AgentConfig {
        mcp_tools: vec![],
        model: "gpt-scripted".into(),
        name: "assistant".into(),
        preamble: String::new(),
        provider: ProviderKind::Openai,
    }]
}

async fn make_app(replies: Vec<ScriptedReply>) -> (Router, Arc<AppState<ScriptedPrompter>>) {
    let prompter = Arc::new(ScriptedPrompter::new(make_agents(), replies));
    let config = MemoryConfig {
        backend: BackendConfig::InMemory,
        embedder: EmbedderConfig::Hash { dims: 32 },
        ..MemoryConfig::default()
    };
    let memory = Arc::new(Store::open(config, None).await.unwrap());
    let tracker = Tracker::new();
    let state = Arc::new(AppState {
        default_user_id: None,
        extractor: None,
        memory,
        prompter,
        tracker,
    });
    let server = Server::new(([127, 0, 0, 1], 0).into(), Arc::clone(&state));
    (server.router(), state)
}

fn json_request(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn collect(body: Body) -> Bytes {
    body.collect().await.unwrap().to_bytes()
}

#[tokio::test(flavor = "current_thread")]
async fn non_streaming_returns_openai_shape_and_persists_turn() {
    let (app, state) = make_app(vec![ScriptedReply::text("Hello there").with_usage(Usage {
        input_tokens: 7,
        output_tokens: 3,
        total_tokens: 10,
        ..Usage::default()
    })])
    .await;

    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "Hi"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = collect(resp.into_body()).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["object"], "chat.completion");
    assert_eq!(v["model"], "assistant");
    assert_eq!(v["choices"][0]["message"]["content"], "Hello there");
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    assert_eq!(v["usage"]["total_tokens"], 10);

    // Both messages landed in memory.
    let alice = UserId::from_string("alice");
    let um = state.memory.for_user(alice);
    let messages = um.messages().await.unwrap();
    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0].role, MemRole::User));
    assert_eq!(messages[0].content, "Hi");
    assert!(matches!(messages[1].role, MemRole::Assistant));
    assert_eq!(messages[1].content, "Hello there");
}

#[tokio::test(flavor = "current_thread")]
async fn request_without_user_id_is_rejected() {
    let (app, _) = make_app(vec![ScriptedReply::text("unused")]).await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "messages": [{"role": "user", "content": "Hi"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "current_thread")]
async fn request_without_user_message_is_rejected() {
    let (app, _) = make_app(vec![ScriptedReply::text("unused")]).await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "system", "content": "be brief"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_agent_returns_not_found() {
    let (app, _) = make_app(vec![ScriptedReply::text("unused")]).await;
    let req = json_request(serde_json::json!({
        "model": "does-not-exist",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "Hi"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "current_thread")]
async fn history_is_loaded_on_the_second_request() {
    let (app, state) = make_app(vec![
        ScriptedReply::text("first reply"),
        ScriptedReply::text("second reply"),
    ])
    .await;

    // Turn 1.
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "first"}],
    }));
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    drop(collect(resp.into_body()).await);

    // Turn 2 — the scripted prompter sees the assembled context.
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "second"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    drop(collect(resp.into_body()).await);

    let captured = state.prompter.calls();
    assert_eq!(captured.len(), 2);
    let turn2 = &captured[1];
    // Should contain: turn-1 user ("first"), turn-1 assistant ("first reply"),
    // turn-2 user ("second"). At minimum the "first" user message is present.
    let contents: Vec<&str> = turn2.iter().map(|m| m.content.as_str()).collect();
    assert!(contents.contains(&"first"), "turn 2 saw: {contents:?}");
    assert!(
        contents.contains(&"first reply"),
        "turn 2 saw: {contents:?}"
    );
    assert!(contents.contains(&"second"), "turn 2 saw: {contents:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn users_cannot_see_each_others_history() {
    let (app, state) = make_app(vec![ScriptedReply::text("reply")]).await;

    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "alice-secret"}],
    }));
    let _ = app.clone().oneshot(req).await.unwrap();

    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "bob",
        "messages": [{"role": "user", "content": "bob-question"}],
    }));
    let _ = app.oneshot(req).await.unwrap();

    let captured = state.prompter.calls();
    assert_eq!(captured.len(), 2);
    let bob_call = &captured[1];
    for m in bob_call {
        assert!(
            !m.content.contains("alice-secret"),
            "bob's context leaked alice's message: {:?}",
            m.content
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn models_endpoint_lists_configured_agents() {
    let (app, _) = make_app(vec![]).await;
    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = collect(resp.into_body()).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["object"], "list");
    assert_eq!(v["data"][0]["id"], "assistant");
}

// ---------- Streaming ----------

fn parse_sse(bytes: &[u8]) -> Vec<String> {
    std::str::from_utf8(bytes)
        .unwrap()
        .split("\n\n")
        .filter_map(|block| {
            let line = block.lines().find(|l| l.starts_with("data:"))?;
            Some(line.trim_start_matches("data:").trim().to_string())
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_emits_role_content_stop_and_done_in_order() {
    let (app, _) = make_app(vec![ScriptedReply::deltas(["Hel", "lo", " world"])]).await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true,
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = collect(resp.into_body()).await;
    let frames = parse_sse(&body);

    // Expect: role chunk, 3 content chunks, stop chunk, [DONE].
    assert_eq!(frames.len(), 6, "frames: {frames:?}");
    let role: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(role["choices"][0]["delta"]["role"], "assistant");

    let collected: String = (1..=3)
        .map(|i| {
            let v: serde_json::Value = serde_json::from_str(&frames[i]).unwrap();
            v["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(collected, "Hello world");

    let stop: serde_json::Value = serde_json::from_str(&frames[4]).unwrap();
    assert_eq!(stop["choices"][0]["finish_reason"], "stop");
    assert!(
        stop.get("usage").is_none(),
        "usage should be omitted by default"
    );

    assert_eq!(frames[5], "[DONE]");
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_include_usage_puts_usage_on_terminal_chunk() {
    let (app, _) = make_app(vec![ScriptedReply::deltas(["hi"]).with_usage(Usage {
        input_tokens: 4,
        output_tokens: 1,
        total_tokens: 5,
        ..Usage::default()
    })])
    .await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "ping"}],
        "stream": true,
        "stream_options": {"include_usage": true},
    }));
    let resp = app.oneshot(req).await.unwrap();
    let body = collect(resp.into_body()).await;
    let frames = parse_sse(&body);

    let stop: serde_json::Value = serde_json::from_str(&frames[frames.len() - 2]).unwrap();
    assert_eq!(stop["choices"][0]["finish_reason"], "stop");
    assert_eq!(stop["usage"]["total_tokens"], 5);
    assert_eq!(stop["usage"]["prompt_tokens"], 4);
    assert_eq!(stop["usage"]["completion_tokens"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_persists_full_assistant_message_on_normal_completion() {
    let (app, state) = make_app(vec![ScriptedReply::deltas(["one ", "two ", "three"])]).await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "count"}],
        "stream": true,
    }));
    let resp = app.oneshot(req).await.unwrap();
    let _ = collect(resp.into_body()).await;

    // The Drop guard spawns a task; give the runtime a moment to drain it.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let um = state.memory.for_user(UserId::from_string("alice"));
    let messages = um.messages().await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "count");
    assert_eq!(messages[1].content, "one two three");
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_persists_partial_message_when_client_disconnects() {
    use futures::StreamExt;

    let (app, state) = make_app(vec![ScriptedReply::deltas([
        "piece-one",
        "piece-two",
        "piece-three",
    ])])
    .await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "go"}],
        "stream": true,
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Read one frame then drop the body — simulates a client disconnect.
    let mut body = resp.into_body().into_data_stream();
    let _first = body.next().await.unwrap().unwrap();
    drop(body);

    // Let the Drop guard's spawned task persist whatever we accumulated.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let um = state.memory.for_user(UserId::from_string("alice"));
    let messages = um.messages().await.unwrap();
    // User message must be persisted. Assistant may be partial or full.
    assert!(
        !messages.is_empty(),
        "no messages persisted after disconnect"
    );
    assert_eq!(messages[0].content, "go");
    if messages.len() > 1 {
        let assistant = &messages[1].content;
        assert!(
            assistant.starts_with("piece-one"),
            "unexpected assistant text: {assistant:?}"
        );
    }
}
