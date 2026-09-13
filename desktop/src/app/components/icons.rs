use leptos::prelude::*;

/// A single 24px line-icon family; all icons inherit text color.
#[component]
pub fn Icon(#[prop(into)] name: String, #[prop(optional)] size: u32) -> impl IntoView {
    let path = match name.as_str() {
        "home" => "M3 10 12 3l9 7v10a1 1 0 0 1-1 1h-5v-7H9v7H4a1 1 0 0 1-1-1Z",
        "video" => "M14 8 21 4v16l-7-4M3 5h10a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Z",
        "folder" => "M3 5h6l2 2h10v13H3Z M3 10h18",
        "settings" => "m9 3-1 3-3 1-2 3 2 2-1 3 3 3 3-1 2 2 3-2 1-3 3-1 1-4-3-2V5l-3-2-2 2Z M9 12a3 3 0 1 0 6 0 3 3 0 0 0-6 0",
        "info" => "M12 11v6m0-10v.01M3 12a9 9 0 1 0 18 0 9 9 0 0 0-18 0",
        "plus" => "M12 5v14M5 12h14",
        "search" => "m21 21-4.5-4.5M3 10a7 7 0 1 0 14 0 7 7 0 0 0-14 0",
        "grid" => "M3 3h7v7H3ZM14 3h7v7h-7ZM3 14h7v7H3ZM14 14h7v7h-7Z",
        "list" => "M8 5h13M8 12h13M8 19h13M3 5h.01M3 12h.01M3 19h.01",
        "play" => "m8 4 12 8-12 8Z",
        "pause" => "M8 4v16M16 4v16",
        "stop" => "M5 5h14v14H5Z",
        "refresh" => "M20 7V3m0 4h-4M4 17v4m0-4h4M20 7a9 9 0 0 0-15-2M4 17a9 9 0 0 0 15 2",
        "edit" => "m15 5 4 4M4 20l4-1L21 6a2.8 2.8 0 0 0-4-4L4 15ZM13 20h8",
        "trash" => "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7",
        "eye" => "M2 12s4-7 10-7 10 7 10 7-4 7-10 7-10-7-10-7Zm7 0a3 3 0 1 0 6 0 3 3 0 0 0-6 0",
        "arrow" => "M5 12h14m-5-5 5 5-5 5",
        "chevron" => "m9 5 7 7-7 7",
        "back" => "M19 12H5m5-5-5 5 5 5",
        "close" => "m6 6 12 12M6 18 18 6",
        "minimize" => "M5 12h14",
        "maximize" => "M5 5h14v14H5Z",
        "restore" => "M8 8h11v11H8ZM5 15V5h10",
        "tray" => "M3 14v6h18v-6M12 3v12m-5-5 5 5 5-5",
        "power" => "M12 2v10M6 5a9 9 0 1 0 12 0",
        "check" => "m5 12 4 4L19 6",
        "sun" => "M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5M8 12a4 4 0 1 0 8 0 4 4 0 0 0-8 0",
        "moon" => "M20.5 13A9 9 0 0 1 11 3a9 9 0 1 0 9.5 10Z",
        "monitor" => "M3 3h18v13H3ZM8 21h8m-4-5v5",
        "signal" => "M4 20v-3m5 3v-7m5 7V9m5 11V4",
        "clock" => "M12 7v5l3 2M3 12a9 9 0 1 0 18 0 9 9 0 0 0-18 0",
        "link" => "m10 13 4-4M8 16l-2 2a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0m4 0 2-2a4 4 0 0 1 6 6l-4 4a4 4 0 0 1-6 0",
        "shield" => "m12 2 9 4v6c0 5-9 10-9 10S3 17 3 12V6Zm-4 9 3 3 5-5",
        "alert" => "m12 3 10 18H2Zm0 5v5m0 4v.01",
        "copy" => "M8 8h13v13H8ZM16 8V3H3v13h5",
        "external" => "M14 3h7v7m0-7L10 14M10 3H3v18h18v-7",
        "download" => "M12 3v12m-5-5 5 5 5-5M3 16v5h18v-5",
        "file" => "M5 2h9l5 5v15H5Zm9 0v6h5M8 13h8M8 17h5",
        "volume" => "m3 9 4 0 5-5v16l-5-5H3Zm13-2a7 7 0 0 1 0 10m3-13a11 11 0 0 1 0 16",
        "lock" => "M5 10h14v11H5Zm3 0V6a4 4 0 0 1 8 0v4m-4 4v3",
        "qr" => "M3 3h6v6H3ZM15 3h6v6h-6ZM3 15h6v6H3ZM15 15h3v3h3v3h-6Zm3-3h3",
        "spark" => "m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5Z",
        _ => "M4 12h16",
    };
    let size = if size == 0 { 20 } else { size };
    view! { <svg class="icon" width=size height=size viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path /></svg> }
}
