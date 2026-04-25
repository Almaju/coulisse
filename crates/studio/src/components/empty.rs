use leptos::prelude::*;

#[component]
pub fn Empty(#[prop(into)] message: String) -> impl IntoView {
    view! {
        <div class="flex items-center justify-center rounded-lg border border-dashed border-slate-800 bg-slate-950/50 px-6 py-10 text-sm text-slate-400">
            {message}
        </div>
    }
}
