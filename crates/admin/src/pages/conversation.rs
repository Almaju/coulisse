use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use uuid::Uuid;

use crate::api::{self, MemoryView, MessageView, Role};
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
            <div>
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
    view! {
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
