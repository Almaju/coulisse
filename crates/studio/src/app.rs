use leptos::prelude::*;
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::path;

use crate::pages::{ConversationPage, UsersPage};

#[component]
pub fn App() -> impl IntoView {
    view! {
        <Router base="/studio">
            <div class="flex min-h-screen flex-col bg-slate-950">
                <Header/>
                <main class="mx-auto w-full max-w-6xl flex-1 px-6 py-8">
                    <Routes fallback=|| view! { <NotFound/> }>
                        <Route path=path!("/") view=UsersPage/>
                        <Route path=path!("/users/:user_id") view=ConversationPage/>
                    </Routes>
                </main>
            </div>
        </Router>
    }
}

#[component]
fn Header() -> impl IntoView {
    view! {
        <header class="border-b border-slate-800 bg-slate-900/60 backdrop-blur">
            <div class="mx-auto flex w-full max-w-6xl items-center justify-between px-6 py-4">
                <A href="/" attr:class="flex items-center gap-2">
                    <span class="inline-flex h-7 w-7 items-center justify-center rounded-md bg-slate-100 text-xs font-bold text-slate-900">
                        "C"
                    </span>
                    <span class="text-sm font-semibold text-slate-100">"Coulisse studio"</span>
                </A>
                <span class="text-xs text-slate-500">"Read-only · no auth"</span>
            </div>
        </header>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <div class="rounded-lg border border-slate-800 bg-slate-900 px-6 py-10 text-center text-slate-400">
            "Page not found."
        </div>
    }
}
