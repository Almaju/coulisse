use leptos::prelude::*;

#[component]
pub fn Spinner() -> impl IntoView {
    view! {
        <div class="flex items-center gap-2 text-sm text-slate-400">
            <span class="inline-block h-3 w-3 animate-spin rounded-full border-2 border-slate-700 border-t-slate-200"></span>
            <span>"Loading…"</span>
        </div>
    }
}
