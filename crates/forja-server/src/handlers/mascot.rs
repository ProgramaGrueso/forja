use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

const MASCOT_FILES: [(&str, &[u8]); 5] = [
    ("idle.webp", include_bytes!("../../assets/mascot/idle.webp")),
    (
        "blink.webp",
        include_bytes!("../../assets/mascot/blink.webp"),
    ),
    (
        "working.webp",
        include_bytes!("../../assets/mascot/working.webp"),
    ),
    (
        "celebrate.webp",
        include_bytes!("../../assets/mascot/celebrate.webp"),
    ),
    (
        "worried.webp",
        include_bytes!("../../assets/mascot/worried.webp"),
    ),
];

/// Bytes de un sprite de la mascota embebido (p. ej. `idle.webp`), para el icono
/// del lanzador de escritorio.
pub fn mascot_bytes(name: &str) -> Option<&'static [u8]> {
    MASCOT_FILES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, bytes)| *bytes)
}

pub(crate) async fn mascot_handler(Path(name): Path<String>) -> Response {
    match MASCOT_FILES.iter().find(|(n, _)| *n == name) {
        Some((_, bytes)) => (
            [
                (header::CONTENT_TYPE, "image/webp"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            *bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
