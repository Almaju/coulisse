use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use uuid::Uuid;

use crate::api::{
    self, CriterionAverage, MemoryView, MessageView, Role, ScoreView, ScoresResponse, ToolCallKind,
    ToolCallView,
};
use crate::components::{Badge, Card, CardContent, CardHeader, CardTitle, Empty, Spinner};
use crate::pages::relative_time;

#[component]
pub fn ConversationPage() -> impl IntoView {
    let params = use_params_map();
    let parsed = Memo::new(move |_| {
        params
            .read()
            .get("user_id")
            .and_then(|raw| Uuid::parse_str(&raw).ok())
    });

    view! {
        <div class="space-y-6">
            <A href="/" attr:class="inline-flex items-center gap-1 text-sm text-slate-400 hover:text-slate-100">
                <span>"←"</span>
                <span>"Back to users"</span>
            </A>
            {move || match parsed.get() {
                None => view! {
                    <Empty message="Invalid user id in URL.".to_string()/>
                }.into_any(),
                Some(user_id) => view! { <ConversationView user_id=user_id/> }.into_any(),
            }}
        </div>
    }
}

#[component]
fn ConversationView(user_id: Uuid) -> impl IntoView {
    let (messages, set_messages) = signal::<Load<Vec<MessageView>>>(Load::Loading);
    let (memories, set_memories) = signal::<Load<Vec<MemoryView>>>(Load::Loading);
    let (scores, set_scores) = signal::<Load<ScoresResponse>>(Load::Loading);

    spawn_local(async move {
        match api::user_messages(user_id).await {
            Ok(m) => set_messages.set(Load::Ready(m)),
            Err(e) => set_messages.set(Load::Failed(e.to_string())),
        }
    });
    spawn_local(async move {
        match api::user_memories(user_id).await {
            Ok(m) => set_memories.set(Load::Ready(m)),
            Err(e) => set_memories.set(Load::Failed(e.to_string())),
        }
    });
    spawn_local(async move {
        match api::user_scores(user_id).await {
            Ok(s) => set_scores.set(Load::Ready(s)),
            Err(e) => set_scores.set(Load::Failed(e.to_string())),
        }
    });

    view! {
        <div>
            <h1 class="text-2xl font-semibold tracking-tight text-slate-100">"Conversation"</h1>
            <p class="mt-1 font-mono text-xs text-slate-400">{user_id.to_string()}</p>
        </div>
        <div class="grid grid-cols-1 gap-6 lg:grid-cols-3">
            <div class="lg:col-span-2">
                <Card>
                    <CardHeader>
                        <CardTitle>"Messages"</CardTitle>
                    </CardHeader>
                    <CardContent>
                        {move || match messages.get() {
                            Load::Loading => view! { <Spinner/> }.into_any(),
                            Load::Failed(msg) => view! {
                                <Empty message=format!("Failed to load messages: {msg}")/>
                            }.into_any(),
                            Load::Ready(msgs) if msgs.is_empty() => view! {
                                <Empty message="No messages recorded for this user.".to_string()/>
                            }.into_any(),
                            Load::Ready(msgs) => view! { <MessageList items=msgs/> }.into_any(),
                        }}
                    </CardContent>
                </Card>
            </div>
            <div class="space-y-6">
                <Card>
                    <CardHeader>
                        <CardTitle>"Scores"</CardTitle>
                    </CardHeader>
                    <CardContent>
                        {move || match scores.get() {
                            Load::Loading => view! { <Spinner/> }.into_any(),
                            Load::Failed(msg) => view! {
                                <Empty message=format!("Failed to load scores: {msg}")/>
                            }.into_any(),
                            Load::Ready(s) if s.scores.is_empty() => view! {
                                <Empty message="No judge scores recorded for this user.".to_string()/>
                            }.into_any(),
                            Load::Ready(s) => view! { <ScoresPanel data=s/> }.into_any(),
                        }}
                    </CardContent>
                </Card>
                <Card>
                    <CardHeader>
                        <CardTitle>"Memories"</CardTitle>
                    </CardHeader>
                    <CardContent>
                        {move || match memories.get() {
                            Load::Loading => view! { <Spinner/> }.into_any(),
                            Load::Failed(msg) => view! {
                                <Empty message=format!("Failed to load memories: {msg}")/>
                            }.into_any(),
                            Load::Ready(mems) if mems.is_empty() => view! {
                                <Empty message="No long-term memories for this user.".to_string()/>
                            }.into_any(),
                            Load::Ready(mems) => view! { <MemoryList items=mems/> }.into_any(),
                        }}
                    </CardContent>
                </Card>
            </div>
        </div>
    }
}

#[component]
fn ScoresPanel(data: ScoresResponse) -> impl IntoView {
    let averages = data.averages;
    let recent: Vec<ScoreView> = data.scores.into_iter().rev().take(5).collect();
    view! {
        <div class="space-y-4">
            <div>
                <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-slate-400">
                    "Averages"
                </h3>
                <AveragesList items=averages/>
            </div>
            <div>
                <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-slate-400">
                    "Recent"
                </h3>
                <RecentScoresList items=recent/>
            </div>
        </div>
    }
}

#[component]
fn AveragesList(items: Vec<CriterionAverage>) -> impl IntoView {
    let rows: Vec<_> = items
        .into_iter()
        .map(|a| {
            view! {
                <div class="flex items-center justify-between rounded-md border border-slate-800 bg-slate-950/60 px-3 py-2">
                    <div class="min-w-0">
                        <div class="truncate text-sm text-slate-200">{a.criterion}</div>
                        <div class="text-xs text-slate-500">
                            {format!("{} · n={}", a.judge_name, a.count)}
                        </div>
                    </div>
                    <div class="ml-3 font-mono text-sm text-slate-100">
                        {format!("{:.1}", a.average)}
                    </div>
                </div>
            }
        })
        .collect();
    view! { <div class="space-y-2">{rows}</div> }
}

#[component]
fn RecentScoresList(items: Vec<ScoreView>) -> impl IntoView {
    let rows: Vec<_> = items
        .into_iter()
        .map(|s| {
            view! {
                <div class="rounded-md border border-slate-800 bg-slate-950/60 px-3 py-2">
                    <div class="mb-1 flex items-center justify-between">
                        <div class="min-w-0">
                            <span class="text-sm text-slate-200">{s.criterion}</span>
                            <span class="ml-1 text-xs text-slate-500">
                                {format!("· {}", s.judge_name)}
                            </span>
                        </div>
                        <div class="flex items-center gap-2">
                            <span class="font-mono text-sm text-slate-100">
                                {format!("{:.1}", s.score)}
                            </span>
                            <span class="text-xs text-slate-500">
                                {relative_time(s.created_at)}
                            </span>
                        </div>
                    </div>
                    <p class="text-xs text-slate-400">{s.reasoning}</p>
                </div>
            }
        })
        .collect();
    view! { <div class="space-y-2">{rows}</div> }
}

#[component]
fn MessageList(items: Vec<MessageView>) -> impl IntoView {
    let rows: Vec<_> = items
        .into_iter()
        .map(|m| view! { <MessageRow m=m/> })
        .collect();
    view! { <div class="space-y-3">{rows}</div> }
}

#[component]
fn MessageRow(m: MessageView) -> impl IntoView {
    let tone = match m.role {
        Role::Assistant => "bg-indigo-950/40 border-indigo-900/60",
        Role::System => "bg-slate-950/60 border-slate-800",
        Role::User => "bg-slate-900 border-slate-800",
    };
    let label_class = match m.role {
        Role::Assistant => "text-indigo-300",
        Role::System => "text-slate-500",
        Role::User => "text-slate-300",
    };
    // Render tool calls BEFORE the assistant text so the UI walks the turn
    // in the order it actually happened: user → (tool call → result)* → text.
    let mut tool_calls = m.tool_calls;
    tool_calls.sort_by_key(|t| t.ordinal);
    let tool_call_rows: Vec<_> = tool_calls
        .into_iter()
        .map(|t| view! { <ToolCallBlock t=t/> })
        .collect();
    view! {
        <div class="space-y-2">
            {tool_call_rows}
            <div class=format!("rounded-lg border px-4 py-3 {tone}")>
                <div class="flex items-center justify-between">
                    <span class=format!("text-xs font-semibold uppercase tracking-wide {label_class}")>
                        {m.role.label()}
                    </span>
                    <div class="flex items-center gap-2 text-xs text-slate-500">
                        <span>{format!("{} tok", m.token_count)}</span>
                        <span>"·"</span>
                        <span>{relative_time(m.created_at)}</span>
                    </div>
                </div>
                <pre class="mt-2 whitespace-pre-wrap break-words font-sans text-sm text-slate-200">
                    {m.content}
                </pre>
            </div>
        </div>
    }
}

#[component]
fn ToolCallBlock(t: ToolCallView) -> impl IntoView {
    let (kind_label, kind_class) = match t.kind {
        ToolCallKind::Mcp => ("mcp", "bg-amber-950/60 text-amber-300 border-amber-900/60"),
        ToolCallKind::Subagent => (
            "subagent",
            "bg-emerald-950/60 text-emerald-300 border-emerald-900/60",
        ),
    };
    let outcome_label = if t.error.is_some() {
        "error"
    } else if t.result.is_some() {
        "result"
    } else {
        "pending"
    };
    view! {
        <details class="rounded-md border border-slate-800 bg-slate-950/80 px-3 py-2 text-sm text-slate-300">
            <summary class="flex cursor-pointer items-center justify-between gap-2">
                <div class="flex items-center gap-2 min-w-0">
                    <span class=format!("rounded border px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide {kind_class}")>
                        {kind_label}
                    </span>
                    <span class="truncate font-mono text-xs text-slate-200">{t.tool_name.clone()}</span>
                </div>
                <span class="text-xs text-slate-500">{outcome_label}</span>
            </summary>
            <div class="mt-2 space-y-2">
                <div>
                    <div class="text-[10px] font-semibold uppercase tracking-wide text-slate-500">"Args"</div>
                    <pre class="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-slate-950 px-2 py-1 font-mono text-xs text-slate-300">
                        {t.args}
                    </pre>
                </div>
                {match (t.result, t.error) {
                    (_, Some(err)) => view! {
                        <div>
                            <div class="text-[10px] font-semibold uppercase tracking-wide text-rose-400">"Error"</div>
                            <pre class="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-slate-950 px-2 py-1 font-mono text-xs text-rose-300">
                                {err}
                            </pre>
                        </div>
                    }.into_any(),
                    (Some(res), None) => view! {
                        <div>
                            <div class="text-[10px] font-semibold uppercase tracking-wide text-slate-500">"Result"</div>
                            <pre class="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-slate-950 px-2 py-1 font-mono text-xs text-slate-300">
                                {res}
                            </pre>
                        </div>
                    }.into_any(),
                    (None, None) => view! {
                        <div class="text-[10px] italic text-slate-500">"No result recorded (stream may have ended before the tool returned)."</div>
                    }.into_any(),
                }}
            </div>
        </details>
    }
}

#[component]
fn MemoryList(items: Vec<MemoryView>) -> impl IntoView {
    let rows: Vec<_> = items
        .into_iter()
        .map(|m| {
            view! {
                <div class="rounded-md border border-slate-800 bg-slate-950/60 px-3 py-2">
                    <div class="mb-1 flex items-center justify-between">
                        <Badge>{m.kind.label()}</Badge>
                        <span class="text-xs text-slate-500">{relative_time(m.created_at)}</span>
                    </div>
                    <p class="text-sm text-slate-200">{m.content}</p>
                </div>
            }
        })
        .collect();
    view! { <div class="space-y-2">{rows}</div> }
}

#[derive(Clone, Debug)]
enum Load<T> {
    Failed(String),
    Loading,
    Ready(T),
}
