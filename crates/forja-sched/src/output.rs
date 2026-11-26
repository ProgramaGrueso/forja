//! Buffer de salida en vivo por ejecución (en memoria, con tope).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use forja_core::redact_secrets;
use tokio::sync::watch;

/// Tope de chunks (líneas) retenidos por ejecución.
pub const MAX_CHUNKS: usize = 2000;
/// Tope de bytes de texto retenidos por ejecución.
pub const MAX_BYTES: usize = 512 * 1024;
/// Máximo de bytes de una sola línea (el resto se trunca).
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Fragmento de salida ya redactado. `seq` empieza en 0 y crece sin huecos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputChunk {
    pub seq: u64,
    /// `"stdout"` o `"stderr"`.
    pub stream: String,
    pub text: String,
}

/// Página de salida para reconectar: `next_seq` es el valor a pasar en `since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPage {
    pub chunks: Vec<OutputChunk>,
    pub next_seq: u64,
    /// `true` si hay chunks anteriores a `since` que ya se descartaron por el tope.
    pub truncated: bool,
    /// `true` cuando el run terminó y no llegarán más chunks.
    pub finished: bool,
}

struct State {
    chunks: VecDeque<OutputChunk>,
    bytes: usize,
    next_seq: u64,
    finished: bool,
}

pub(crate) struct RunOutput {
    state: Mutex<State>,
    notify: watch::Sender<u64>,
    max_chunks: usize,
    max_bytes: usize,
}

impl RunOutput {
    pub(crate) fn new(max_chunks: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                chunks: VecDeque::new(),
                bytes: 0,
                next_seq: 0,
                finished: false,
            }),
            notify: watch::channel(0).0,
            max_chunks,
            max_bytes,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Redacta y añade un chunk. Ignora texto vacío o si ya terminó.
    pub(crate) fn push(&self, stream: &str, text: &str) {
        let mut text = redact_secrets(text);
        if text.len() > MAX_LINE_BYTES {
            let mut cut = MAX_LINE_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("…[truncado]\n");
        }
        if text.is_empty() {
            return;
        }
        let mut st = self.lock();
        if st.finished {
            return;
        }
        let seq = st.next_seq;
        st.next_seq += 1;
        st.bytes += text.len();
        st.chunks.push_back(OutputChunk {
            seq,
            stream: stream.to_string(),
            text,
        });
        while st.chunks.len() > self.max_chunks
            || (st.bytes > self.max_bytes && st.chunks.len() > 1)
        {
            if let Some(old) = st.chunks.pop_front() {
                st.bytes -= old.text.len();
            }
        }
        let version = st.next_seq;
        drop(st);
        self.notify.send_replace(version);
    }

    pub(crate) fn finish(&self) {
        let mut st = self.lock();
        if st.finished {
            return;
        }
        st.finished = true;
        let version = st.next_seq + 1;
        drop(st);
        self.notify.send_replace(version);
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.lock().finished
    }

    pub(crate) fn page(&self, since: u64) -> OutputPage {
        let st = self.lock();
        let first = st.chunks.front().map_or(st.next_seq, |c| c.seq);
        OutputPage {
            chunks: st
                .chunks
                .iter()
                .filter(|c| c.seq >= since)
                .cloned()
                .collect(),
            next_seq: st.next_seq,
            truncated: since < first,
            finished: st.finished,
        }
    }

    pub(crate) fn watch(&self) -> watch::Receiver<u64> {
        self.notify.subscribe()
    }
}

/// Valida un id de run: solo `[A-Za-z0-9_-]`, 1..=64 caracteres.
pub fn is_valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Suscripción a la salida de un run: backlog + espera de nuevos chunks.
pub struct OutputSubscription {
    pub(crate) out: Arc<RunOutput>,
    pub(crate) rx: watch::Receiver<u64>,
    pub(crate) next_seq: u64,
    pub(crate) done: bool,
}

impl OutputSubscription {
    /// Devuelve el siguiente lote de chunks (esperando si no hay). `None` cuando
    /// el run terminó y se entregó todo.
    pub async fn next_batch(&mut self) -> Option<Vec<OutputChunk>> {
        loop {
            let page = self.out.page(self.next_seq);
            if !page.chunks.is_empty() {
                self.next_seq = page.next_seq;
                return Some(page.chunks);
            }
            if page.finished {
                self.done = true;
                return None;
            }
            // Si el emisor desaparece, el run ya no puede producir más.
            if self.rx.changed().await.is_err() {
                self.done = true;
                return None;
            }
        }
    }
}
