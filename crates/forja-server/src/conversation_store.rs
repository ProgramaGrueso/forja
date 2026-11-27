//! Persistencia de las conversaciones del orquestador en `<store_dir>/conversations.json`.
//!
//! La escritura es atómica (archivo temporal en el mismo directorio + `rename`) y corre en
//! un hilo de fondo con coalescencia para no bloquear el turno de chat ni mantener locks.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use crate::orchestrator::{Message, MAX_CONVERSATIONS, MAX_MESSAGES, PRINCIPAL};

/// Nombre del archivo persistido dentro de `<store_dir>`, junto a `runs.jsonl`.
pub const CONVERSATIONS_FILE: &str = "conversations.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_CONSECUTIVE_FAILURES: u8 = 2;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Estado persistido de una conversación. No incluye `in_flight`, `pending` ni `auto_turns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedConversation {
    pub id: String,
    pub titulo: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub historial: Vec<Message>,
    pub creado: u64,
    pub actualizado: u64,
    #[serde(default)]
    pub modelo: Option<String>,
    #[serde(default)]
    pub esfuerzo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedFile {
    version: u32,
    conversations: Vec<PersistedConversation>,
}

/// Aplica los invariantes de límites:
/// - Descarta IDs vacíos y resuelve duplicados quedándose con el `actualizado` más reciente.
/// - Conserva siempre la conversación `principal` si está presente.
/// - Como máximo `MAX_CONVERSATIONS` conversaciones (descartando las más antiguas por `actualizado`).
/// - Como máximo `MAX_MESSAGES` mensajes por conversación (descartando los más viejos).
pub fn enforce_limits(conversations: Vec<PersistedConversation>) -> Vec<PersistedConversation> {
    let mut by_id: HashMap<String, PersistedConversation> = HashMap::new();
    for mut conv in conversations {
        let id = conv.id.trim().to_string();
        if id.is_empty() {
            continue;
        }
        conv.id = id.clone();
        if conv.historial.len() > MAX_MESSAGES {
            let excess = conv.historial.len() - MAX_MESSAGES;
            conv.historial.drain(..excess);
        }
        match by_id.get(&id) {
            Some(existing) if existing.actualizado > conv.actualizado => {}
            _ => {
                by_id.insert(id, conv);
            }
        }
    }

    let principal = by_id.remove(PRINCIPAL);
    let mut others: Vec<PersistedConversation> = by_id.into_values().collect();
    others.sort_by(|a, b| {
        b.actualizado
            .cmp(&a.actualizado)
            .then_with(|| a.id.cmp(&b.id))
    });

    let max_others = MAX_CONVERSATIONS.saturating_sub(1);
    if others.len() > max_others {
        others.truncate(max_others);
    }

    let mut out = Vec::with_capacity(others.len() + usize::from(principal.is_some()));
    if let Some(p) = principal {
        out.push(p);
    }
    out.extend(others);
    out.sort_by(|a, b| {
        b.actualizado
            .cmp(&a.actualizado)
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Carga las conversaciones desde `path`.
///
/// Si el archivo no existe, devuelve vacío sin avisos. Si está corrupto o alguna entrada
/// es inválida, ignora solo la parte dañada, emite un único aviso por log y nunca falla.
pub fn load_conversations(path: &Path) -> Vec<PersistedConversation> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            eprintln!(
                "forja: aviso: no se pudo leer {}: {err}; arrancando sin conversaciones guardadas",
                path.display()
            );
            return Vec::new();
        }
    };

    let root: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "forja: aviso: {} está corrupto ({err}); arrancando solo con la conversación principal",
                path.display()
            );
            return Vec::new();
        }
    };

    let Some(entries) = root.get("conversations").and_then(|v| v.as_array()) else {
        eprintln!(
            "forja: aviso: formato inválido en {}; arrancando solo con la conversación principal",
            path.display()
        );
        return Vec::new();
    };

    let mut valid = Vec::with_capacity(entries.len());
    let mut invalid = 0usize;
    for entry in entries {
        match serde_json::from_value::<PersistedConversation>(entry.clone()) {
            Ok(conv) if !conv.id.trim().is_empty() => valid.push(conv),
            _ => invalid += 1,
        }
    }

    if invalid > 0 {
        eprintln!(
            "forja: aviso: se ignoraron {invalid} entrada(s) inválida(s) en {}",
            path.display()
        );
    }

    enforce_limits(valid)
}

/// Escribe las conversaciones de forma atómica (temporal en el mismo directorio + `rename`),
/// aplicando los límites antes de serializar.
pub fn write_atomic(path: &Path, conversations: &[PersistedConversation]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let payload = PersistedFile {
        version: SCHEMA_VERSION,
        conversations: enforce_limits(conversations.to_vec()),
    };
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|err| std::io::Error::new(ErrorKind::InvalidData, err))?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(CONVERSATIONS_FILE);
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}.{seq}", std::process::id()));

    let write_res = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    Ok(())
}

struct WriterState {
    pending: Option<Vec<PersistedConversation>>,
    requested_rev: u64,
    completed_rev: u64,
    consecutive_failures: u8,
    shutdown: bool,
}

struct WriterShared {
    state: Mutex<WriterState>,
    cv: Condvar,
    done_cv: Condvar,
    attempts: AtomicU64,
}

/// Escritor de fondo con coalescencia y corte tras 2 fallos seguidos hasta el próximo cambio.
pub struct ConversationPersistence {
    path: PathBuf,
    shared: Arc<WriterShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ConversationPersistence {
    pub fn new(path: PathBuf) -> Self {
        let shared = Arc::new(WriterShared {
            state: Mutex::new(WriterState {
                pending: None,
                requested_rev: 0,
                completed_rev: 0,
                consecutive_failures: 0,
                shutdown: false,
            }),
            cv: Condvar::new(),
            done_cv: Condvar::new(),
            attempts: AtomicU64::new(0),
        });

        let thread_shared = Arc::clone(&shared);
        let thread_path = path.clone();
        let handle = std::thread::Builder::new()
            .name("forja-conv-store".to_string())
            .spawn(move || writer_loop(thread_path, thread_shared))
            .ok();

        Self {
            path,
            shared,
            worker: Mutex::new(handle),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Programa un guardado coalescido sin bloquear al llamante.
    pub fn schedule(&self, snapshot: Vec<PersistedConversation>) {
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        st.pending = Some(snapshot);
        st.requested_rev = st.requested_rev.saturating_add(1);
        st.consecutive_failures = 0;
        self.shared.cv.notify_one();
    }

    /// Espera a que termine la escritura correspondiente a todos los cambios encolados hasta ahora.
    pub fn flush(&self) {
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let target = st.requested_rev;
        while st.completed_rev < target {
            st = self
                .shared
                .done_cv
                .wait(st)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    #[cfg(test)]
    pub(crate) fn write_attempts(&self) -> u64 {
        self.shared.attempts.load(Ordering::Relaxed)
    }
}

impl Drop for ConversationPersistence {
    fn drop(&mut self) {
        {
            let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            st.shutdown = true;
            self.shared.cv.notify_one();
        }
        if let Some(handle) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

fn writer_loop(path: PathBuf, shared: Arc<WriterShared>) {
    loop {
        let (snapshot, rev) = {
            let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while st.pending.is_none() && !st.shutdown {
                st = shared.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            if st.pending.is_none() && st.shutdown {
                break;
            }
            let Some(snapshot) = st.pending.take() else {
                continue;
            };
            let rev = st.requested_rev;
            (snapshot, rev)
        };

        shared.attempts.fetch_add(1, Ordering::Relaxed);
        match write_atomic(&path, &snapshot) {
            Ok(()) => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                st.consecutive_failures = 0;
                if rev > st.completed_rev {
                    st.completed_rev = rev;
                }
                shared.done_cv.notify_all();
            }
            Err(err) => {
                eprintln!(
                    "forja: aviso: no se pudieron guardar las conversaciones en {}: {err}",
                    path.display()
                );
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                st.consecutive_failures = st.consecutive_failures.saturating_add(1);
                if st.consecutive_failures < MAX_CONSECUTIVE_FAILURES
                    && st.pending.is_none()
                    && !st.shutdown
                {
                    st.pending = Some(snapshot);
                } else if st.pending.is_none() {
                    if rev > st.completed_rev {
                        st.completed_rev = rev;
                    }
                    shared.done_cv.notify_all();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn conv(id: &str, updated: u64, msgs: usize) -> PersistedConversation {
        PersistedConversation {
            id: id.to_string(),
            titulo: format!("Titulo {id}"),
            session_id: Some(format!("sess-{id}")),
            historial: (0..msgs)
                .map(|i| Message {
                    rol: if i % 2 == 0 {
                        "usuario".to_string()
                    } else {
                        "orquestador".to_string()
                    },
                    texto: format!("msg-{i}"),
                    ts: updated + i as u64,
                })
                .collect(),
            creado: 100,
            actualizado: updated,
            modelo: Some("sonnet".to_string()),
            esfuerzo: Some("high".to_string()),
        }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = TempDir::new().unwrap();
        let loaded = load_conversations(&dir.path().join(CONVERSATIONS_FILE));
        assert!(loaded.is_empty());
    }

    #[test]
    fn partial_corruption_ignores_only_bad_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(CONVERSATIONS_FILE);
        let raw = r#"{
            "version": 1,
            "conversations": [
                {"id": "principal", "titulo": "Principal", "historial": [], "creado": 1, "actualizado": 10},
                {"id": "", "titulo": "Vacio", "historial": [], "creado": 1, "actualizado": 20},
                {"roto": true},
                {"id": "c1-abc", "titulo": "Valida", "historial": [{"rol": "usuario", "texto": "hola", "ts": 5}], "creado": 2, "actualizado": 30}
            ]
        }"#;
        fs::write(&path, raw).unwrap();
        let loaded = load_conversations(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "c1-abc");
        assert_eq!(loaded[1].id, PRINCIPAL);
    }

    #[test]
    fn enforce_limits_keeps_principal_and_newest_20_and_500_msgs() {
        let mut items = vec![conv(PRINCIPAL, 1, 600)];
        for i in 0..25 {
            items.push(conv(&format!("c{i}"), 100 + i as u64, 510));
        }
        let trimmed = enforce_limits(items);
        assert_eq!(trimmed.len(), MAX_CONVERSATIONS);
        assert!(
            trimmed.iter().any(|c| c.id == PRINCIPAL),
            "principal siempre se conserva aunque sea la más antigua"
        );
        // Las 19 más recientes son c6..=c24; c0..=c5 se descartan.
        assert!(!trimmed.iter().any(|c| c.id == "c0"));
        assert!(!trimmed.iter().any(|c| c.id == "c5"));
        assert!(trimmed.iter().any(|c| c.id == "c6"));
        assert!(trimmed.iter().any(|c| c.id == "c24"));

        let p = trimmed.iter().find(|c| c.id == PRINCIPAL).unwrap();
        assert_eq!(p.historial.len(), MAX_MESSAGES);
        assert_eq!(p.historial.first().unwrap().texto, "msg-100");
        assert_eq!(p.historial.last().unwrap().texto, "msg-599");
    }

    #[test]
    fn stops_retrying_after_two_consecutive_failures_until_next_change() {
        let dir = TempDir::new().unwrap();
        // Un directorio en la ruta destino provoca error de rename/escritura siempre.
        let bad_path = dir.path().join("conversations.json");
        fs::create_dir(&bad_path).unwrap();

        let persistence = ConversationPersistence::new(bad_path.clone());
        persistence.schedule(vec![conv(PRINCIPAL, 10, 1)]);
        persistence.flush();
        assert_eq!(
            persistence.write_attempts(),
            2,
            "debe fallar dos veces seguidas y detenerse"
        );

        // Un nuevo cambio vuelve a intentarlo (otros 2 intentos).
        persistence.schedule(vec![conv(PRINCIPAL, 20, 2)]);
        persistence.flush();
        assert_eq!(persistence.write_attempts(), 4);

        // Si el destino vuelve a ser válido, el siguiente cambio guarda correctamente.
        fs::remove_dir(&bad_path).unwrap();
        persistence.schedule(vec![conv(PRINCIPAL, 30, 3)]);
        persistence.flush();
        assert_eq!(persistence.write_attempts(), 5);
        let loaded = load_conversations(&bad_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].actualizado, 30);
    }
}
