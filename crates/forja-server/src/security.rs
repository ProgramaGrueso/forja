//! Defensa contra CSRF y DNS rebinding para el servidor HTTP local.
//!
//! Aunque Forja escucha solo en loopback, un navegador puede ser engañado para
//! hablar con él:
//!
//! * **DNS rebinding**: una página de `evil.example` resuelve su dominio a
//!   `127.0.0.1` y llama a la API; la petición llega con `Host: evil.example`.
//! * **CSRF**: una página de otro origen envía un `POST`/`PUT`/`DELETE` (por
//!   ejemplo un formulario) con las credenciales locales del navegador.
//!
//! Este middleware valida, en todas las rutas (panel, `/api/*`, SSE y mascota):
//!
//! * `Host` siempre: debe ser `localhost`, `127.0.0.1` o `[::1]` con el puerto
//!   real del servidor. Sin `Host` también se rechaza.
//! * `Origin`, si viene: debe ser `http://` + uno de esos hosts locales con el
//!   mismo puerto. `Origin: null`, otros esquemas, hosts o puertos → 403.
//! * `Sec-Fetch-Site: cross-site` se rechaza como defensa adicional.
//!
//! Las peticiones sin `Origin` (curl, agentes, scripts locales) se permiten:
//! los navegadores modernos siempre envían `Origin` en peticiones mutantes o
//! cross-origin, así que no es un vector CSRF. Forja no expone WebSockets; el
//! streaming usa SSE, que también pasa por esta validación.

use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Nombres de host aceptados, ya normalizados a minúsculas.
const LOCAL_HOSTNAMES: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// Middleware: valida `Host`/`Origin` contra el puerto real del servidor.
pub(crate) async fn guard(port: u16, req: Request, next: Next) -> Response {
    let headers = req.headers();

    if !host_is_allowed(headers, port) {
        return forbidden("host no permitido (posible DNS rebinding)");
    }
    if !origin_is_allowed(headers, port) {
        return forbidden("origen no permitido (posible CSRF)");
    }
    if is_cross_site(headers) {
        return forbidden("petición cross-site rechazada");
    }

    next.run(req).await
}

fn forbidden(reason: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": reason }))).into_response()
}

/// `Host` es obligatorio, único y debe ser un host local con el puerto del servidor.
fn host_is_allowed(headers: &HeaderMap, port: u16) -> bool {
    let mut values = headers.get_all(header::HOST).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    value
        .to_str()
        .map(|host| host_is_local(host, port))
        .unwrap_or(false)
}

/// `Origin` es opcional, pero si viene debe ser único y local.
fn origin_is_allowed(headers: &HeaderMap, port: u16) -> bool {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(value) = values.next() else {
        return true;
    };
    if values.next().is_some() {
        return false;
    }
    value
        .to_str()
        .map(|origin| origin_is_local(origin, port))
        .unwrap_or(false)
}

fn is_cross_site(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
}

/// `host[:puerto]` con un nombre local y el puerto esperado.
fn host_is_local(host: &str, port: u16) -> bool {
    let Some((name, parsed_port)) = split_host_port(host.trim()) else {
        return false;
    };
    let port_matches = match parsed_port {
        Some(p) => p == port,
        // Solo se admite omitir el puerto si el servidor usa el puerto por defecto.
        None => port == 80,
    };
    port_matches && is_local_hostname(name)
}

/// Origen serializado del navegador: `http://host[:puerto]`, sin ruta ni consulta.
fn origin_is_local(origin: &str, port: u16) -> bool {
    let origin = origin.trim();
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    if rest.contains(['/', '?', '#']) {
        return false;
    }
    host_is_local(rest, port)
}

fn is_local_hostname(name: &str) -> bool {
    LOCAL_HOSTNAMES
        .iter()
        .any(|local| name.eq_ignore_ascii_case(local))
}

/// Separa `nombre:puerto`, respetando la forma IPv6 `[::1]:puerto`.
fn split_host_port(host: &str) -> Option<(&str, Option<u16>)> {
    if host.is_empty() {
        return None;
    }
    if let Some(rest) = host.strip_prefix('[') {
        let end = rest.find(']')?;
        let name_len = end + 2; // '[' + contenido + ']'
        let name = &host[..name_len];
        let tail = &host[name_len..];
        if tail.is_empty() {
            return Some((name, None));
        }
        let port = tail.strip_prefix(':')?.parse().ok()?;
        return Some((name, Some(port)));
    }
    match host.rsplit_once(':') {
        Some((name, raw_port))
            if !raw_port.is_empty() && raw_port.chars().all(|c| c.is_ascii_digit()) =>
        {
            Some((name, Some(raw_port.parse().ok()?)))
        }
        Some(_) => None,
        None => Some((host, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_local_hosts_with_server_port() {
        for host in [
            "localhost:7777",
            "127.0.0.1:7777",
            "[::1]:7777",
            "LOCALHOST:7777",
        ] {
            assert!(host_is_local(host, 7777), "{host} debe aceptarse");
        }
    }

    #[test]
    fn rejects_foreign_hosts_ports_and_malformed_values() {
        for host in [
            "evil.example:7777",
            "localhost.evil.example:7777",
            "127.0.0.1.evil.example:7777",
            "127.0.0.2:7777",
            "localhost",
            "127.0.0.1:9999",
            "localhost:7777.evil",
            "localhost:",
            "[::2]:7777",
            "",
        ] {
            assert!(!host_is_local(host, 7777), "{host} debe rechazarse");
        }
    }

    #[test]
    fn accepts_only_http_local_origins_with_server_port() {
        for origin in [
            "http://localhost:7777",
            "http://127.0.0.1:7777",
            "http://[::1]:7777",
            "HTTP://LOCALHOST:7777",
        ] {
            assert!(origin_is_local(origin, 7777), "{origin} debe aceptarse");
        }
        for origin in [
            "http://evil.example:7777",
            "https://localhost:7777",
            "http://localhost:9999",
            "http://127.0.0.1:7777/",
            "http://127.0.0.1:7777?x=1",
            "null",
            "file://localhost",
            "",
        ] {
            assert!(!origin_is_local(origin, 7777), "{origin} debe rechazarse");
        }
    }

    #[test]
    fn portless_host_only_valid_on_default_port() {
        assert!(host_is_local("localhost", 80));
        assert!(origin_is_local("http://localhost", 80));
        assert!(!host_is_local("localhost", 7777));
    }
}
