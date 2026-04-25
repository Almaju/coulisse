//! HTTP-level integration tests for the chat completions endpoint. Drives
//! the real axum router through `tower::ServiceExt::oneshot` against a
//! `ScriptedPrompter` — no network, no real provider. The streaming tests
//! also exercise the `MemoryFlush` Drop guard by dropping the response body
//! mid-stream.
//!
//! Tests use the `current_thread` flavor so a 20ms sleep after dropping a
//! response is enough for the spawned memory-flush task to complete.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use config::{AgentConfig, JudgeConfig, ProviderKind};
use http_body_util::BodyExt;
use judge::Judge;
use limits::Tracker;
use memory::{BackendConfig, EmbedderConfig, MemoryConfig, Role as MemRole, Store, UserId};
use prompter::testing::{ScriptedPrompter, ScriptedReply};
use prompter::{ToolCallKind, Usage};
use server::{AppState, Server};
use tower::ServiceExt;

fn agent_with_judges(judges: Vec<String>) -> AgentConfig {
    AgentConfig {
        judges,
        mcp_tools: vec![],
        model: "gpt-scripted".into(),
        name: "assistant".into(),
        preamble: String::new(),
        provider: ProviderKind::Openai,
        purpose: None,
        subagents: vec![],
    }
}

fn make_agents() -> Vec<AgentConfig> {
    vec![agent_with_judges(vec![])]
}

async fn make_app(replies: Vec<ScriptedReply>) -> (Router, Arc<AppState<ScriptedPrompter>>) {
    make_app_with(make_agents(), HashMap::new(), replies).await
}

async fn make_app_with(
    agents: Vec<AgentConfig>,
    judges: HashMap<String, Arc<Judge>>,
    replies: Vec<ScriptedReply>,
) -> (Router, Arc<AppState<ScriptedPrompter>>) {
    make_app_full(agents, judges, replies, None).await
}

async fn make_app_full(
    agents: Vec<AgentConfig>,
    judges: HashMap<String, Arc<Judge>>,
    replies: Vec<ScriptedReply>,
    studio_auth: Option<server::StudioAuth>,
) -> (Router, Arc<AppState<ScriptedPrompter>>) {
    let prompter = Arc::new(ScriptedPrompter::new(agents, replies));
    let config = MemoryConfig {
        backend: BackendConfig::InMemory,
        embedder: EmbedderConfig::Hash { dims: 32 },
        ..MemoryConfig::default()
    };
    let memory = Arc::new(Store::open(config, None).await.unwrap());
    let tracker = Tracker::open(memory.pool().clone()).await.unwrap();
    let telemetry = Arc::new(telemetry::Sink::open(memory.pool().clone()).await.unwrap());
    let state = Arc::new(AppState {
        studio_auth,
        default_user_id: None,
        extractor: None,
        judges: Arc::new(judges),
        memory,
        prompter,
        telemetry,
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
async fn judge_scores_are_persisted_after_a_turn() {
    // Two scripted replies: the assistant's answer and the judge's JSON.
    let judge_reply = r#"{
        "accuracy": {"score": 8, "reasoning": "mostly right"},
        "helpfulness": {"score": 9, "reasoning": "answered the question"}
    }"#;
    let replies = vec![
        ScriptedReply::text("The capital of France is Paris."),
        ScriptedReply::text(judge_reply),
    ];
    let rubrics: BTreeMap<String, String> = [
        ("accuracy".into(), "factual correctness".into()),
        ("helpfulness".into(), "answered the question".into()),
    ]
    .into_iter()
    .collect();
    let judge = Judge::from_config(&JudgeConfig {
        model: "gpt-scripted".into(),
        name: "quality".into(),
        provider: "openai".into(),
        rubrics,
        sampling_rate: 1.0,
    })
    .unwrap();
    let mut judges = HashMap::new();
    judges.insert("quality".into(), Arc::new(judge));

    let agents = vec![agent_with_judges(vec!["quality".into()])];
    let (app, state) = make_app_with(agents, judges, replies).await;

    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "What is the capital of France?"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    drop(collect(resp.into_body()).await);

    // Give the spawned judge task time to persist.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let um = state.memory.for_user(UserId::from_string("alice"));
    let mut scores = um.scores().await.unwrap();
    scores.sort_by(|a, b| a.criterion.cmp(&b.criterion));
    assert_eq!(scores.len(), 2);
    assert_eq!(scores[0].criterion, "accuracy");
    assert_eq!(scores[0].score, 8.0);
    assert_eq!(scores[0].reasoning, "mostly right");
    assert_eq!(scores[0].judge_name, "quality");
    assert_eq!(scores[1].criterion, "helpfulness");
    assert_eq!(scores[1].score, 9.0);
}

#[tokio::test(flavor = "current_thread")]
async fn judge_scores_are_persisted_after_a_streaming_turn() {
    let judge_reply = r#"{
        "helpfulness": {"score": 7, "reasoning": "ok"}
    }"#;
    let replies = vec![
        ScriptedReply::deltas(["Hel", "lo"]),
        ScriptedReply::text(judge_reply),
    ];
    let rubrics: BTreeMap<String, String> = [("helpfulness".into(), "answered".into())]
        .into_iter()
        .collect();
    let judge = Judge::from_config(&JudgeConfig {
        model: "gpt-scripted".into(),
        name: "quality".into(),
        provider: "openai".into(),
        rubrics,
        sampling_rate: 1.0,
    })
    .unwrap();
    let mut judges = HashMap::new();
    judges.insert("quality".into(), Arc::new(judge));

    let agents = vec![agent_with_judges(vec!["quality".into()])];
    let (app, state) = make_app_with(agents, judges, replies).await;

    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true,
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = collect(resp.into_body()).await;

    // MemoryFlush spawns on Drop; the judge spawns again from inside.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let um = state.memory.for_user(UserId::from_string("alice"));
    let scores = um.scores().await.unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].criterion, "helpfulness");
    assert_eq!(scores[0].score, 7.0);
}

#[tokio::test(flavor = "current_thread")]
async fn judge_sampling_rate_zero_records_nothing() {
    let rubrics: BTreeMap<String, String> = [("accuracy".into(), "factual".into())]
        .into_iter()
        .collect();
    let judge = Judge::from_config(&JudgeConfig {
        model: "gpt-scripted".into(),
        name: "q".into(),
        provider: "openai".into(),
        rubrics,
        sampling_rate: 0.0,
    })
    .unwrap();
    let mut judges = HashMap::new();
    judges.insert("q".into(), Arc::new(judge));

    let agents = vec![agent_with_judges(vec!["q".into()])];
    let replies = vec![ScriptedReply::text("answer")];
    let (app, state) = make_app_with(agents, judges, replies).await;

    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "Q"}],
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    drop(collect(resp.into_body()).await);

    tokio::time::sleep(Duration::from_millis(30)).await;

    let um = state.memory.for_user(UserId::from_string("alice"));
    assert_eq!(um.scores().await.unwrap().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_persists_tool_calls_attached_to_assistant_message() {
    let reply = ScriptedReply::text("Found it.")
        .with_tool_call(
            "web_search",
            r#"{"q":"capital of France"}"#,
            ToolCallKind::Mcp,
            Some("Paris is the capital of France.".into()),
        )
        .with_tool_call(
            "specialist_agent",
            r#"{"message":"verify"}"#,
            ToolCallKind::Subagent,
            Some("Verified: Paris.".into()),
        );
    let (app, state) = make_app(vec![reply]).await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "capital of France?"}],
        "stream": true,
    }));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    drop(collect(resp.into_body()).await);

    // Drop guard spawns the persistence task; give it time to complete.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let um = state.memory.for_user(UserId::from_string("alice"));
    let messages = um.messages().await.unwrap();
    assert_eq!(messages.len(), 2);
    let assistant = &messages[1];
    assert_eq!(assistant.role, MemRole::Assistant);

    let mut tool_calls = um.tool_calls().await.unwrap();
    tool_calls.sort_by_key(|t| t.ordinal);
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].tool_name, "web_search");
    assert_eq!(tool_calls[0].ordinal, 0);
    assert_eq!(tool_calls[0].kind, memory::ToolCallKind::Mcp);
    assert_eq!(tool_calls[0].args, r#"{"q":"capital of France"}"#);
    assert_eq!(
        tool_calls[0].result.as_deref(),
        Some("Paris is the capital of France.")
    );
    assert_eq!(tool_calls[0].message_id, assistant.id);
    assert_eq!(tool_calls[1].tool_name, "specialist_agent");
    assert_eq!(tool_calls[1].ordinal, 1);
    assert_eq!(tool_calls[1].kind, memory::ToolCallKind::Subagent);
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

// ---------- Studio auth ----------

fn basic_auth_header(user: &str, pass: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
    format!("Basic {encoded}")
}

async fn make_app_with_studio_auth(
    user: &str,
    pass: &str,
) -> (Router, Arc<AppState<ScriptedPrompter>>) {
    make_app_full(
        make_agents(),
        HashMap::new(),
        vec![],
        Some(server::StudioAuth::Basic(server::StudioCredentials::new(
            user, pass,
        ))),
    )
    .await
}

fn get_studio(uri: &str, auth: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(h) = auth {
        builder = builder.header("authorization", h);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn studio_api_without_auth_returns_401_with_challenge() {
    let (app, _) = make_app_with_studio_auth("admin", "s3cret").await;
    let resp = app
        .oneshot(get_studio("/studio/api/users", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp
        .headers()
        .get("www-authenticate")
        .expect("challenge header")
        .to_str()
        .unwrap();
    assert!(
        challenge.starts_with("Basic "),
        "unexpected challenge: {challenge}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn studio_api_with_wrong_password_is_rejected() {
    let (app, _) = make_app_with_studio_auth("admin", "s3cret").await;
    let resp = app
        .oneshot(get_studio(
            "/studio/api/users",
            Some(&basic_auth_header("admin", "wrong")),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn studio_api_with_correct_credentials_passes_through() {
    let (app, _) = make_app_with_studio_auth("admin", "s3cret").await;
    let resp = app
        .oneshot(get_studio(
            "/studio/api/users",
            Some(&basic_auth_header("admin", "s3cret")),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "current_thread")]
async fn studio_ui_without_auth_returns_401() {
    let (app, _) = make_app_with_studio_auth("admin", "s3cret").await;
    // Hit an SPA sub-route rather than `/studio/` itself — axum 0.8's nested
    // routers don't normalize trailing slashes, so the root is served via
    // the `{*path}` route that the SPA depends on for its client-side
    // routing anyway.
    let resp = app
        .oneshot(get_studio("/studio/index.html", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn chat_endpoint_remains_unauthenticated_when_studio_auth_is_set() {
    // Basic auth guards /studio only — /v1/chat/completions must stay open
    // to existing OpenAI clients that don't speak Basic.
    let (app, _) = make_app_with_studio_auth("admin", "s3cret").await;
    let req = json_request(serde_json::json!({
        "model": "assistant",
        "safety_identifier": "alice",
        "messages": [{"role": "user", "content": "hi"}],
    }));
    // Scripted prompter has no reply queued; we only care about the status
    // code from the auth middleware, not the completion outcome.
    let resp = app.oneshot(req).await.unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "chat endpoint must not be behind studio auth"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn studio_api_without_configured_auth_is_open() {
    // No `studio_credentials` → no middleware enforcement, matching the
    // pre-auth behavior so existing dev setups keep working.
    let (app, _) = make_app(vec![]).await;
    let resp = app
        .oneshot(get_studio("/studio/api/users", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
