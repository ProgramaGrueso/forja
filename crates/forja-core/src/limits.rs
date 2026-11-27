//! Topes de tamaño compartidos por el planificador, el servidor y la CLI.

/// Tamaño máximo del prompt de una tarea (bytes UTF-8). Los adaptadores pasan el
/// prompt como UN argumento y Linux limita cada argumento a 128 KiB
/// (`MAX_ARG_STRLEN`); este tope deja margen y da un error claro antes de lanzar.
pub const MAX_PROMPT_BYTES: usize = 100 * 1024;
/// Salida final guardada de una ejecución (`RunRecord.output`).
pub const MAX_STORED_OUTPUT_BYTES: usize = 256 * 1024;
/// Error guardado de una ejecución (`RunRecord.error`).
pub const MAX_STORED_ERROR_BYTES: usize = 64 * 1024;

/// Recorta `text` a unos `max_bytes` conservando el principio y el final, con una
/// marca en medio que dice cuántos bytes se omitieron. No parte caracteres UTF-8.
pub fn clip_middle(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let half = max_bytes / 2;
    let mut head = half;
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - half;
    while tail < text.len() && !text.is_char_boundary(tail) {
        tail += 1;
    }
    let omitted = tail - head;
    format!(
        "{}\n…[salida truncada: se omitieron {omitted} bytes]…\n{}",
        &text[..head],
        &text[tail..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_middle_keeps_short_text_and_marks_long_text() {
        assert_eq!(clip_middle("hola", 10), "hola");
        let long = "ñ".repeat(1000);
        let clipped = clip_middle(&long, 101);
        assert!(clipped.len() < 200, "{}", clipped.len());
        assert!(clipped.contains("salida truncada"));
        assert!(clipped.starts_with('ñ') && clipped.ends_with('ñ'));
    }
}
