mod api;
mod app;

use app::AppShell;
use leptos::prelude::*;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::default());
    mount_to_body(|| view! { <AppShell /> })
}
