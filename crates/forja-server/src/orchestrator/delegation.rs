//! Seguimiento de delegaciones, reacciones automáticas al terminar tareas y
//! control de turnos automáticos.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::delegation::Pending;

use super::conversations::lock;
use super::Orchestrator;

/// Turnos automáticos seguidos que puede dar una conversación sin que el usuario
/// escriba. Al agotarse, los resultados se dejan para el siguiente mensaje del
/// usuario: así una cadena de delegaciones nunca puede girar sin fin.
pub const MAX_AUTO_TURNS: usize = 8;
/// Espera máxima a que una conversación quede libre antes de dejar la reacción
/// pendiente para el siguiente turno del usuario.
pub const AUTO_IDLE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Seguimiento de las tareas delegadas, para reaccionar cuando terminan.
#[derive(Default)]
pub(super) struct Reactions {
    /// run_id -> conversación que lo delegó.
    tracked: HashMap<String, String>,
    /// conversación -> run_ids terminados aún no revisados.
    finished: HashMap<String, Vec<String>>,
    /// Conversaciones con una reacción automática encolada o en curso.
    scheduled: HashSet<String>,
}

impl Orchestrator {
    pub(super) fn clear_reactions(&self, conversation: &str) {
        let mut r = lock(&self.reactions);
        r.tracked.retain(|_, conv| conv != conversation);
        r.finished.remove(conversation);
        r.scheduled.remove(conversation);
    }

    /// Registra que una ejecución fue delegada desde `conversation`. Devuelve
    /// `false` si la conversación ya no existe (no hay a quién notificar).
    pub fn track_delegation(&self, conversation: &str, run_id: &str) -> bool {
        let exists = lock(&self.inner).map.contains_key(conversation);
        if !exists {
            return false;
        }
        lock(&self.reactions)
            .tracked
            .insert(run_id.to_string(), conversation.to_string());
        true
    }

    /// Marca una ejecución como terminada. Devuelve la conversación solo la
    /// primera vez: llamarlo dos veces (evento SSE y reconciliación) no duplica.
    pub fn note_run_finished(&self, run_id: &str) -> Option<String> {
        let conversation = lock(&self.reactions).tracked.remove(run_id)?;
        // Si la conversación se borró mientras la tarea corría, no hay aviso.
        if !lock(&self.inner).map.contains_key(&conversation) {
            return None;
        }
        let mut r = lock(&self.reactions);
        let ids = r.finished.entry(conversation.clone()).or_default();
        if !ids.iter().any(|id| id == run_id) {
            ids.push(run_id.to_string());
        }
        Some(conversation)
    }

    /// Reclama el derecho a lanzar una reacción automática para la conversación.
    /// Solo una a la vez; devuelve `false` si no hay nada pendiente o ya hay otra.
    pub fn try_schedule_reaction(&self, conversation: &str) -> bool {
        let exists = lock(&self.inner).map.contains_key(conversation);
        if !exists {
            return false;
        }
        let mut r = lock(&self.reactions);
        if r.finished.get(conversation).is_none_or(Vec::is_empty) {
            return false;
        }
        r.scheduled.insert(conversation.to_string())
    }

    /// Run_ids terminados que aún no se han entregado al orquestador.
    pub fn take_reaction_batch(&self, conversation: &str) -> Vec<String> {
        lock(&self.reactions)
            .finished
            .remove(conversation)
            .unwrap_or_default()
    }

    /// Devuelve un lote al estado pendiente (p. ej. si la conversación estaba
    /// ocupada por un turno del usuario y hay que reintentar después).
    pub fn requeue_reaction(&self, conversation: &str, ids: Vec<String>) {
        let mut r = lock(&self.reactions);
        let pending = r.finished.entry(conversation.to_string()).or_default();
        for id in ids {
            if !pending.contains(&id) {
                pending.push(id);
            }
        }
    }

    /// Libera la marca de reacción en curso.
    pub fn release_reaction(&self, conversation: &str) {
        lock(&self.reactions).scheduled.remove(conversation);
    }

    pub fn has_pending_reaction(&self, conversation: &str) -> bool {
        lock(&self.reactions)
            .finished
            .get(conversation)
            .is_some_and(|ids| !ids.is_empty())
    }

    /// Ejecuciones delegadas que el servidor sigue vigilando.
    pub fn tracked_runs(&self) -> Vec<(String, String)> {
        lock(&self.reactions)
            .tracked
            .iter()
            .map(|(run, conversation)| (run.clone(), conversation.clone()))
            .collect()
    }

    /// Quita de «Tareas delegadas» las tareas que ya se entregan en el aviso
    /// automático, para no repetirlas dos veces en el mismo turno.
    pub fn forget_pending_tasks(&self, conversation: &str, ids: &[String]) {
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(conversation) {
            c.pending
                .retain(|p| !matches!(p, Pending::Task { id, .. } if ids.iter().any(|i| i == id)));
        }
    }

    /// Consume un turno automático si queda margen. Evita cadenas sin fin.
    pub fn begin_auto_turn(&self, conversation: &str) -> bool {
        let mut g = lock(&self.inner);
        match g.map.get_mut(conversation) {
            Some(c) if c.auto_turns < MAX_AUTO_TURNS => {
                c.auto_turns += 1;
                true
            }
            _ => false,
        }
    }

    /// Deshace un turno automático reservado que finalmente no se ejecutó (por
    /// ejemplo, si el usuario ganó la carrera). No debe gastar el límite.
    pub fn abort_auto_turn(&self, conversation: &str) {
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(conversation) {
            c.auto_turns = c.auto_turns.saturating_sub(1);
        }
    }

    /// Deja los resultados para el siguiente mensaje del usuario (límite de
    /// turnos automáticos o espera agotada) y lo explica en el historial.
    ///
    /// Solo retira del lote los resultados diferidos: los que hayan llegado
    /// mientras se preparaba la reacción siguen pendientes y el reactor los
    /// recogerá en la siguiente pasada (si no, se perderían o se repetiría la
    /// misma reacción sin fin).
    pub fn defer_reaction(&self, conversation: &str, runs: &[(String, String)]) {
        let notice = format!(
            "Reacción automática pausada para evitar bucles; {} resultado(s) \
             quedan pendientes para tu próximo mensaje.",
            runs.len()
        );
        let mut pushed = false;
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(conversation) {
            for (id, titulo) in runs {
                let already = c
                    .pending
                    .iter()
                    .any(|p| matches!(p, Pending::Task { id: pid, .. } if pid == id));
                if !already {
                    c.pending.push(Pending::Task {
                        id: id.clone(),
                        titulo: titulo.clone(),
                    });
                }
            }
            c.push("sistema", &notice);
            pushed = true;
        }
        drop(g);
        let mut r = lock(&self.reactions);
        if let Some(pending) = r.finished.get_mut(conversation) {
            pending.retain(|id| !runs.iter().any(|(deferred, _)| deferred == id));
            if pending.is_empty() {
                r.finished.remove(conversation);
            }
        }
        drop(r);
        if pushed {
            self.persist();
        }
    }

    /// Espera a que la conversación no tenga una llamada en curso. `false` si
    /// no existe o se agota `timeout` (no bloquea al vigilante para siempre).
    pub async fn wait_idle(&self, conversation: &str, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let idle = {
                let g = lock(&self.inner);
                match g.map.get(conversation) {
                    None => return false,
                    Some(c) if c.in_flight.is_none() => return true,
                    Some(_) => Arc::clone(&g.idle),
                }
            };
            // `notify_waiters` no guarda permiso: el futuro debe registrarse con
            // `enable` ANTES de la segunda comprobación. Así, si la conversación
            // se libera entre ambas, o el aviso despierta el futuro o la
            // comprobación lo ve, sin ventana de pérdida.
            let notified = idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let g = lock(&self.inner);
                match g.map.get(conversation) {
                    None => return false,
                    Some(c) if c.in_flight.is_none() => return true,
                    Some(_) => {}
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return false;
            }
        }
    }
}
