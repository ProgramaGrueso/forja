//! Redactor mínimo de secretos para salida de procesos antes de emitirla.
//!
//! Sin dependencias: recorre la línea separando "tokens" (letras, dígitos,
//! `_` y `-`) del resto y enmascara:
//! - prefijos de claves conocidos (`sk-`, `ghp_`, `github_pat_`, `xox?-`, `AKIA`, `AIza`, `hf_`);
//! - el token que sigue a la palabra `Bearer`;
//! - tokens largos (>= 32) que mezclan letras y dígitos (no hashes hex de 40/64).

pub const REDACTED: &str = "***";

const KEY_PREFIXES: [&str; 8] = [
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "xox",
    "AKIA",
    "AIza",
    "hf_",
];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn looks_secret(tok: &str) -> bool {
    let len = tok.len();
    if len >= 8 && KEY_PREFIXES.iter().any(|p| tok.starts_with(p)) {
        return true;
    }
    if len >= 32 {
        let all_hex = tok.chars().all(|c| c.is_ascii_hexdigit());
        if all_hex && (len == 40 || len == 64) {
            return false;
        }
        let has_digit = tok.chars().any(|c| c.is_ascii_digit());
        let has_alpha = tok.chars().any(|c| c.is_ascii_alphabetic());
        return has_digit && has_alpha;
    }
    false
}

fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String| {
        if looks_secret(tok) {
            out.push_str(REDACTED);
        } else {
            out.push_str(tok);
        }
        tok.clear();
    };
    for c in text.chars() {
        if is_token_char(c) {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out);
            out.push(c);
        }
    }
    flush(&mut tok, &mut out);
    out
}

/// Enmascara el valor completo (hasta el siguiente espacio) tras `Bearer `.
fn redact_bearer(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = lower[pos..].find("bearer ") {
        let value_start = pos + found + "bearer ".len();
        out.push_str(&text[pos..value_start]);
        let rest = &text[value_start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        if end > 0 {
            out.push_str(REDACTED);
        }
        pos = value_start + end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Devuelve `text` con los secretos reconocibles sustituidos por `***`.
pub fn redact_secrets(text: &str) -> String {
    redact_tokens(&redact_bearer(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_patterns() {
        assert_eq!(redact_secrets("key sk-abcdef1234567890 fin"), "key *** fin");
        assert_eq!(
            redact_secrets("Authorization: Bearer abc.def.ghi"),
            "Authorization: Bearer ***"
        );
        let long = "aZ1".repeat(12);
        assert_eq!(redact_secrets(&format!("t={long}")), "t=***");
        assert_eq!(redact_secrets("ghp_abcdefghij1234"), "***");
    }

    #[test]
    fn keeps_normal_text_and_git_hashes() {
        let s = "compilando forja-core v0.1.0 en /home/u/proyecto";
        assert_eq!(redact_secrets(s), s);
        let sha = "0d23bc8".to_string() + &"a".repeat(33);
        assert_eq!(redact_secrets(&sha), sha);
        assert_eq!(redact_secrets("línea ñ con acentos"), "línea ñ con acentos");
    }
}
