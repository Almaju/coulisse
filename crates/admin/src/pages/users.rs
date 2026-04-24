use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::api::{self, UserView};
use crate::components::{Badge, Card, CardContent, CardHeader, CardTitle, Empty, Spinner};
use crate::pages::relative_time;

#[component]
pub fn UsersPage() -> impl IntoView {
    let (state, set_state) = signal::<Load<Vec<UserView>>>(Load::Loading);

    spawn_local(async move {
        match api::list_users().await {
            Ok(users) => set_state.set(Load::Ready(users)),
            Err(err) => set_state.set(Load::Failed(err.to_string())),
        }
    });

    view! {
        <div class="space-y-6">
            <div>
                <h1 class="text-2xl font-semibold tracking-tight text-slate-100">"Conversations"</h1>
                <p class="mt-1 text-sm text-slate-400">
                    "Every user the server has seen, most recent activity first."
                </p>
            </div>
            <Card>
                <CardHeader>
                    <CardTitle>"Users"</CardTitle>
                </CardHeader>
                <CardContent>
                    {move || match state.get() {
                        Load::Loading => view! { <Spinner/> }.into_any(),
                        Load::Failed(msg) => view! {
                            <Empty message=format!("Failed to load users: {msg}")/>
                        }.into_any(),
                        Load::Ready(users) if users.is_empty() => view! {
                            <Empty message="No users have interacted with the server yet.".to_string()/>
                        }.into_any(),
                        Load::Ready(users) => view! { <UsersTable users=users/> }.into_any(),
                    }}
                </CardContent>
            </Card>
        </div>
    }
}

#[component]
fn UsersTable(users: Vec<UserView>) -> impl IntoView {
    let navigate = use_navigate();
    let rows: Vec<_> = users
        .into_iter()
        .map(|u| {
            let user_id = u.user_id;
            let href = format!("/users/{user_id}");
            let navigate = navigate.clone();
            let on_click = move |_| navigate(&href, Default::default());
            view! {
                <tr
                    class="cursor-pointer border-t border-slate-800 transition-colors hover:bg-slate-800/50"
                    on:click=on_click
                >
                    <td class="px-4 py-3 font-mono text-xs text-slate-300">{user_id.to_string()}</td>
                    <td class="px-4 py-3 text-right text-sm text-slate-300">{u.message_count}</td>
                    <td class="px-4 py-3 text-right text-sm text-slate-300">
                        <Badge>{u.memory_count.to_string()}</Badge>
                    </td>
                    <td class="px-4 py-3 text-right text-sm text-slate-400">
                        {relative_time(u.last_activity_at)}
                    </td>
                </tr>
            }
        })
        .collect();
    view! {
        <div class="overflow-hidden rounded-md border border-slate-800">
            <table class="w-full text-left">
                <thead class="bg-slate-950/60 text-xs uppercase tracking-wide text-slate-400">
                    <tr>
                        <th class="px-4 py-2 font-medium">"User id"</th>
                        <th class="px-4 py-2 text-right font-medium">"Messages"</th>
                        <th class="px-4 py-2 text-right font-medium">"Memories"</th>
                        <th class="px-4 py-2 text-right font-medium">"Last activity"</th>
                    </tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
}

#[derive(Clone, Debug)]
enum Load<T> {
    Failed(String),
    Loading,
    Ready(T),
}
