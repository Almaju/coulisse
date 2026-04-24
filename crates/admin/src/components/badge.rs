use leptos::prelude::*;

#[component]
pub fn Badge(children: Children) -> impl IntoView {
    view! {
        <span class="inline-flex items-center rounded-md bg-slate-800 px-2 py-0.5 text-xs font-medium text-slate-300">
            {children()}
        </span>
    }
}
