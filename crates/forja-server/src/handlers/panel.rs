use axum::response::Html;

const PANEL_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/panel.html"));

pub(crate) async fn panel_handler() -> Html<&'static str> {
    Html(PANEL_HTML)
}
