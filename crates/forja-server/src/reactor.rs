//! Reacción automática del orquestador al terminar una tarea delegada.
//!
//! El planificador emite cada transición de una ejecución por `broadcast`. Este
//! vigilante se queda con el estado final (`done`, `failed` o `cancelled`) de las
//! ejecuciones que el orquestador delegó y le da un turno propio para revisar el
//! resultado, sin esperar a que el usuario escriba.
//!
//! Reglas para no crear bucles ni carreras:
//! - Solo se reacciona a ejecuciones registradas como delegadas (`track_delegation`).
//! - `note_run_finished` consume el registro: el evento SSE y la reconciliación
//!   posterior no pueden disparar dos veces el mismo turno.
//! - Una sola reacción por conversación (`try_schedule_reaction`); los resultados
//!   que llegan mientras tanto se agrupan en el siguiente lote.
//! - Antes de llamar al agente se espera a que la conversación esté libre. Si el
//!   usuario está hablando, el turno automático espera en vez de competir.
//! - Tras [`forja_server::orchestrator::MAX_AUTO_TURNS`] turnos seguidos, los
//!   resultados pasan al siguiente mensaje del usuario. Un usuario que escribe
//!   reinicia el contador.

use std::sync::atomic::Ordering;

use forja_core::{RunRecord, RunStatus};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::orchestrator::{ChatError, AUTO_IDLE_WAIT};
use crate::{delegation, AppState};

/// ¿La ejecución ya no va a cambiar de estado?
pub fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
    )
}

impl AppState {
    /// Arranca el vigilante de ejecuciones una sola vez por estado. Devuelve el
    /// `JoinHandle` la primera vez (útil para abortarlo al apagar el servidor).
    pub fn ensure_run_watcher(&self) -> Option<JoinHandle<()>> {
        if self.watcher_started.swap(true, Ordering::SeqCst) {
            return None;
        }
        let state = self.clone();
        Some(tokio::spawn(async move {
            let mut events = state.scheduler.subscribe();
            loop {
                match events.recv().await {
                    Ok(record) => {
                        if is_terminal(record.status) {
                            state.handle_terminal_run(record);
                        }
                    }
                    // Si el buffer se desbordó, revisar los registros en curso
                    // evita perder un fin de tarea.
                    Err(RecvError::Lagged(_)) => state.reconcile_tracked_runs().await,
                    Err(RecvError::Closed) => break,
                }
            }
        }))
    }

    /// Registra una ejecución delegada y comprueba si ya terminó antes de que
    /// pudiéramos apuntarla (carrera entre `submit` y el alta del seguimiento).
    pub(crate) fn track_delegation(&self, conversation: &str, run_id: &str) {
        if !self.orchestrator.track_delegation(conversation, run_id) {
            return;
        }
        let state = self.clone();
        let conversation = conversation.to_string();
        let run_id = run_id.to_string();
        tokio::spawn(async move {
            state.reconcile_delegation(&conversation, &run_id).await;
        });
    }

    async fn reconcile_delegation(&self, conversation: &str, run_id: &str) {
        if let Some(record) = self.scheduler.get_run(run_id).await {
            if is_terminal(record.status) {
                self.note_finished(run_id, conversation);
            }
        }
    }

    async fn reconcile_tracked_runs(&self) {
        for (run_id, conversation) in self.orchestrator.tracked_runs() {
            if let Some(record) = self.scheduler.get_run(&run_id).await {
                if is_terminal(record.status) {
                    self.note_finished(&run_id, &conversation);
                }
            }
        }
    }

    fn handle_terminal_run(&self, record: RunRecord) {
        if let Some(conversation) = self.orchestrator.note_run_finished(&record.id) {
            self.schedule_reaction(conversation);
        }
    }

    /// Marca una ejecución como terminada y, si corresponde, despierta al
    /// orquestador. Es idempotente: el segundo aviso no encuentra el registro.
    fn note_finished(&self, run_id: &str, _conversation: &str) {
        if let Some(conversation) = self.orchestrator.note_run_finished(run_id) {
            self.schedule_reaction(conversation);
        }
    }

    /// Encola una reacción si no hay otra ya en curso para esa conversación.
    fn schedule_reaction(&self, conversation: String) {
        if !self.orchestrator.try_schedule_reaction(&conversation) {
            return;
        }
        let state = self.clone();
        tokio::spawn(async move {
            state.run_reaction(conversation).await;
        });
    }

    async fn run_reaction(&self, conversation: String) {
        loop {
            if !self
                .orchestrator
                .wait_idle(&conversation, AUTO_IDLE_WAIT)
                .await
            {
                self.defer_pending(&conversation).await;
                break;
            }
            let ids = self.orchestrator.take_reaction_batch(&conversation);
            if ids.is_empty() {
                break;
            }
            if !self.orchestrator.begin_auto_turn(&conversation) {
                // Límite de turnos automáticos: el próximo mensaje del usuario
                // recibirá «Tareas delegadas» con estos resultados.
                self.defer_ids(&conversation, ids).await;
                break;
            }
            // El aviso ya trae el resultado; evita repetirlo en «Tareas delegadas».
            self.orchestrator.forget_pending_tasks(&conversation, &ids);
            let runs = self.runs_for(&ids).await;
            // Datos sensibles solo para orquestadores en sensitive_agents (A5).
            let reveal = delegation::orchestrator_sees_sensitive(
                &self.orchestrator.agent(),
                &self.scheduler.sensitive_agents(),
            );
            let notice = delegation::completion_notice(&runs, reveal);
            let port = self.settings.read().unwrap_or_else(|e| e.into_inner()).port;
            let result = self
                .orchestrator
                .auto_review(
                    &conversation,
                    &notice,
                    port,
                    self.auth_token.as_deref(),
                    Some(self),
                )
                .await;
            match result {
                Ok(_) => {}
                Err(ChatError::Busy) => {
                    // El usuario ganó la carrera por la conversación: reintentar
                    // cuando quede libre, sin perder el lote ni gastar el turno.
                    self.orchestrator.abort_auto_turn(&conversation);
                    self.orchestrator.requeue_reaction(&conversation, ids);
                    continue;
                }
                Err(ChatError::Cancelled) => {
                    // El usuario empezó a hablar y dio prioridad a su turno: el
                    // turno automático no se ejecutó, no debe gastar el límite.
                    self.orchestrator.abort_auto_turn(&conversation);
                    self.defer_ids(&conversation, ids).await;
                    break;
                }
                Err(ChatError::NotFound) => break,
                Err(e) => {
                    self.orchestrator.abort_auto_turn(&conversation);
                    self.orchestrator.push_system(
                        &conversation,
                        &format!(
                            "No se pudo revisar automáticamente el resultado: {e}. \
                             Queda pendiente para el próximo mensaje."
                        ),
                    );
                    self.defer_ids(&conversation, ids).await;
                    break;
                }
            }
            if !self.orchestrator.has_pending_reaction(&conversation) {
                break;
            }
        }
        self.orchestrator.release_reaction(&conversation);
        // Si llegó otro fin de tarea mientras terminaba, encadenar un lote más.
        if self.orchestrator.has_pending_reaction(&conversation) {
            self.schedule_reaction(conversation);
        }
    }

    async fn runs_for(&self, ids: &[String]) -> Vec<RunRecord> {
        let mut runs = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(record) = self.scheduler.get_run(id).await {
                runs.push(record);
            }
        }
        runs
    }

    async fn defer_pending(&self, conversation: &str) {
        let ids = self.orchestrator.take_reaction_batch(conversation);
        if ids.is_empty() {
            return;
        }
        self.defer_ids(conversation, ids).await;
    }

    async fn defer_ids(&self, conversation: &str, ids: Vec<String>) {
        let runs = self.runs_for(&ids).await;
        let items: Vec<(String, String)> = if runs.is_empty() {
            ids.into_iter().map(|id| (id.clone(), id)).collect()
        } else {
            runs.iter()
                .map(|run| (run.id.clone(), run.task.titulo.clone()))
                .collect()
        };
        self.orchestrator.defer_reaction(conversation, &items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_only_done_failed_cancelled() {
        assert!(!is_terminal(RunStatus::Pending));
        assert!(!is_terminal(RunStatus::Running));
        assert!(is_terminal(RunStatus::Done));
        assert!(is_terminal(RunStatus::Failed));
        assert!(is_terminal(RunStatus::Cancelled));
    }
}
