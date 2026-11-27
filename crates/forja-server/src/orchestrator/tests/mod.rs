//! Helpers y dobles compartidos para las pruebas del orquestador.

use std::sync::{Arc, Mutex, RwLock};

use forja_agent::hermes::ProfileRules;

use crate::settings::Settings;

pub(super) use super::conversations::{lock, InFlight};
pub(super) use super::*;

mod backends;
mod commands;
mod conversations;
mod delegation_flow;
mod http;
mod persistence;

pub(super) struct Fake(pub(super) Mutex<Vec<(String, Option<String>)>>);

impl ChatBackend for Fake {
    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            self.0.lock().unwrap().push((prompt, resume));
            Ok(ChatReply {
                text: "hecho".into(),
                session_id: Some("s1".into()),
            })
        })
    }
}

/// Doble con perfil, para probar el cambio en caliente.
pub(super) struct Profiled(pub(super) RwLock<String>);

impl ChatBackend for Profiled {
    fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            Ok(ChatReply {
                text: "ok".into(),
                session_id: Some("s1".into()),
            })
        })
    }
    fn profile(&self) -> Option<String> {
        Some(self.0.read().unwrap().clone())
    }
    fn set_profile(&self, p: &str) {
        *self.0.write().unwrap() = p.to_string();
    }
}

/// Base de Hermes falsa con perfiles: los tests nunca leen el ~/.hermes real.
pub(super) fn fake_rules(profiles: &[&str]) -> (tempfile::TempDir, ProfileRules) {
    let dir = tempfile::TempDir::new().unwrap();
    for p in profiles {
        std::fs::create_dir_all(dir.path().join("profiles").join(p)).unwrap();
    }
    let rules = ProfileRules::new().with_base(dir.path());
    (dir, rules)
}

pub(super) fn fake_script() -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fake_orch_agent.sh")
        .to_string_lossy()
        .into_owned()
}

pub(super) fn base_settings() -> Settings {
    Settings::with_defaults("/tmp/forja-test-store")
}

pub(super) fn only(list: &'static [&'static str]) -> impl Fn(&str) -> bool + Send + Sync {
    move |a: &str| list.contains(&a)
}

/// Doble que da una sesión distinta por conversación nueva y reutiliza la recibida.
pub(super) struct SessionFake {
    n: Mutex<u32>,
    pub(super) calls: Mutex<Vec<(String, Option<String>)>>,
}

impl SessionFake {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            n: Mutex::new(0),
            calls: Mutex::new(vec![]),
        })
    }
}

impl ChatBackend for SessionFake {
    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            let sid = resume.clone().unwrap_or_else(|| {
                let mut n = self.n.lock().unwrap();
                *n += 1;
                format!("sess-{n}")
            });
            self.calls.lock().unwrap().push((prompt, resume));
            Ok(ChatReply {
                text: "ok".into(),
                session_id: Some(sid),
            })
        })
    }
}

/// Doble que bloquea hasta que `n` llamadas están en vuelo a la vez.
pub(super) struct BarrierFake(pub(super) tokio::sync::Barrier);

impl ChatBackend for BarrierFake {
    fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            self.0.wait().await;
            Ok(ChatReply {
                text: "par".into(),
                session_id: None,
            })
        })
    }
}

/// Doble que nunca responde (para `/stop`).
pub(super) struct HangFake;

impl ChatBackend for HangFake {
    fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(std::future::pending())
    }
}
