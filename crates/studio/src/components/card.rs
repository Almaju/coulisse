use leptos::prelude::*;

#[component]
pub fn Card(children: Children) -> impl IntoView {
    view! {
        <div class="rounded-xl border border-slate-800 bg-slate-900 shadow-sm">
            {children()}
        </div>
    }
}

#[component]
pub fn CardHeader(children: Children) -> impl IntoView {
    view! {
        <div class="flex flex-col space-y-1.5 px-6 py-4 border-b border-slate-800">
            {children()}
        </div>
    }
}

#[component]
pub fn CardTitle(children: Children) -> impl IntoView {
    view! {
        <h3 class="font-semibold leading-none tracking-tight text-slate-100">
            {children()}
        </h3>
    }
}

#[component]
pub fn CardContent(children: Children) -> impl IntoView {
    view! {
        <div class="px-6 py-4">
            {children()}
        </div>
    }
}
