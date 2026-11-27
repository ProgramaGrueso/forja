use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::output::RunOutput;

/// Máximo de buffers de salida de runs terminados que se conservan en memoria.
const MAX_RETAINED_OUTPUTS: usize = 256;

#[derive(Default)]
struct OutputBuffersState {
    map: HashMap<String, Arc<RunOutput>>,
    order: VecDeque<String>,
}

#[derive(Default)]
pub(crate) struct OutputBuffers {
    state: Mutex<OutputBuffersState>,
}

impl OutputBuffers {
    pub(crate) fn get(&self, run_id: &str) -> Option<Arc<RunOutput>> {
        let bufs = self.state.lock().unwrap_or_else(|e| e.into_inner());
        bufs.map.get(run_id).cloned()
    }

    /// Registra un buffer y descarta los más antiguos ya terminados si hay demasiados.
    pub(crate) fn register(&self, run_id: &str, out: Arc<RunOutput>) -> Arc<RunOutput> {
        let mut bufs = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = bufs.map.get(run_id) {
            return Arc::clone(existing);
        }
        bufs.map.insert(run_id.to_string(), Arc::clone(&out));
        bufs.order.push_back(run_id.to_string());
        let mut idx = 0;
        while bufs.map.len() > MAX_RETAINED_OUTPUTS && idx < bufs.order.len() {
            let id = bufs.order[idx].clone();
            if bufs.map.get(&id).is_some_and(|o| o.is_finished()) && id != run_id {
                bufs.map.remove(&id);
                bufs.order.remove(idx);
            } else {
                idx += 1;
            }
        }
        out
    }
}

pub(crate) fn push_lines(out: &RunOutput, stream: &str, text: &str) {
    for line in text.split_inclusive('\n') {
        out.push(stream, line);
    }
}
