use axum::response::Html;

const PANEL_HTML: &str = include_str!("../panel.html");

pub(crate) async fn panel_handler() -> Html<&'static str> {
    Html(PANEL_HTML)
}
