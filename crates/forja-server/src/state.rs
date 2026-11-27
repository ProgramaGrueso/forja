use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use forja_agent::hermes::ProfileRules;
use forja_agent::{
    Agent, AgyAgent, ClaudeAgent, HermesAgent, LocalAgent, MockAgent, OpencodeAgent,
};
use forja_core::{AgentKind, ForjaError, Result, RunRecord, Task};
use forja_sched::Scheduler;

use crate::api::AgentMeta;
use crate::conversation_store;
use crate::handlers::CreateTaskRequest;
use crate::models::ModelService;
use crate::orchestrator;
use crate::quota::{default_manual_path, QuotaService};
use crate::settings::Settings;
use crate::telemetry::Telemetry;

#[derive(Clone)]
pub struct AppState {
    pub scheduler: Scheduler,
    agents: Arc<HashMap<AgentKind, Arc<dyn Agent>>>,
    pub(crate) metas: Arc<Vec<AgentMeta>>,
    pub telemetry: Telemetry,
    pub quota: Arc<QuotaService>,
    pub models: Arc<ModelService>,
    pub settings: Arc<RwLock<Settings>>,
    pub settings_path: Option<PathBuf>,
    pub auth_token: Option<String>,
    pub orchestrator: orchestrator::Orchestrator,
    /// Reglas de perfiles de Hermes (descubrimiento y ocultos) del agente y de /api/models.
    pub profile_rules: ProfileRules,
    /// Serializa diff/fusión/descarte para no operar dos veces sobre el mismo repo.
    pub(crate) review_lock: Arc<tokio::sync::Mutex<()>>,
    /// Evita arrancar más de un vigilante de ejecuciones por estado.
    pub(crate) watcher_started: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(scheduler: Scheduler) -> Self {
        let mut agents: HashMap<AgentKind, Arc<dyn Agent>> = HashMap::new();
        agents.insert(AgentKind::Mock, Arc::new(MockAgent::default()));
        agents.insert(AgentKind::Agy, Arc::new(AgyAgent::default()));
        agents.insert(AgentKind::Claude, Arc::new(ClaudeAgent::default()));
        agents.insert(AgentKind::Opencode, Arc::new(OpencodeAgent::default()));
        let profile_rules = ProfileRules::new();
        agents.insert(
            AgentKind::Hermes,
            Arc::new(HermesAgent::default().with_rules(profile_rules.clone())),
        );
        agents.insert(AgentKind::Local, Arc::new(LocalAgent::default()));

        let metas = vec![
            AgentMeta::default_for(AgentKind::Agy, true),
            AgentMeta::default_for(AgentKind::Claude, true),
            AgentMeta::default_for(AgentKind::Hermes, true),
            AgentMeta::default_for(AgentKind::Local, true),
            AgentMeta::default_for(AgentKind::Mock, true),
            AgentMeta::default_for(AgentKind::Opencode, true),
        ];

        let default_quota = default_manual_path();
        let sensitive_agents = scheduler.sensitive_agents();
        let settings = Settings {
            max_concurrency: 2,
            store_dir: ".forja/store".to_string(),
            port: 7777,
            quota_path: default_quota.to_string_lossy().into_owned(),
            hide_mascot: false,
            agent_models: Default::default(),
            agent_effort: Default::default(),
            orchestrator_profile: None,
            orchestrator_agent: None,
            hidden_profiles: Vec::new(),
            sensitive_agents,
        };

        let conversations_path = scheduler
            .store_dir()
            .join(conversation_store::CONVERSATIONS_FILE);

        Self {
            scheduler,
            agents: Arc::new(agents),
            metas: Arc::new(metas),
            telemetry: Telemetry::with_defaults(),
            quota: Arc::new(QuotaService::with_defaults()),
            models: Arc::new(ModelService::with_defaults()),
            settings: Arc::new(RwLock::new(settings)),
            settings_path: None,
            auth_token: None,
            orchestrator: orchestrator::Orchestrator::from_env()
                .with_persistence(conversations_path),
            profile_rules,
            review_lock: Arc::new(tokio::sync::Mutex::new(())),
            watcher_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Reglas de perfiles propias (tests con una base de Hermes falsa): las usan el
    /// agente hermes, el orquestador y /api/models.
    pub fn with_profile_rules(mut self, rules: ProfileRules) -> Self {
        Arc::make_mut(&mut self.agents).insert(
            AgentKind::Hermes,
            Arc::new(HermesAgent::default().with_rules(rules.clone())),
        );
        self.orchestrator = self.orchestrator.clone().with_rules(rules.clone());
        self.profile_rules = rules;
        self
    }

    pub fn with_orchestrator(mut self, orchestrator: orchestrator::Orchestrator) -> Self {
        self.orchestrator = orchestrator;
        self
    }

    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        let t = token.into();
        self.auth_token = if t.trim().is_empty() { None } else { Some(t) };
        self
    }

    pub fn with_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        let kind = agent.kind();
        Arc::make_mut(&mut self.agents).insert(kind, agent);
        let metas = Arc::make_mut(&mut self.metas);
        if let Some(meta) = metas.iter_mut().find(|m| m.kind == kind) {
            meta.available = true;
        } else {
            metas.push(AgentMeta::default_for(kind, true));
        }
        self
    }

    /// Quita un agente (queda como no configurado).
    pub fn without_agent(mut self, kind: AgentKind) -> Self {
        Arc::make_mut(&mut self.agents).remove(&kind);
        if let Some(meta) = Arc::make_mut(&mut self.metas)
            .iter_mut()
            .find(|m| m.kind == kind)
        {
            meta.available = false;
        }
        self
    }

    pub fn with_telemetry(mut self, telemetry: Telemetry) -> Self {
        self.telemetry = telemetry;
        self
    }

    pub fn with_quota(mut self, quota: Arc<QuotaService>) -> Self {
        self.quota = quota;
        self
    }

    pub fn with_models(mut self, models: Arc<ModelService>) -> Self {
        self.models = models;
        self
    }

    pub fn with_settings(self, settings: Settings) -> Self {
        self.quota
            .set_manual_path(PathBuf::from(&settings.quota_path));
        let _ = self
            .scheduler
            .set_sensitive_agents(settings.sensitive_agents.clone());
        self.profile_rules.set_hidden(&settings.hidden_profiles);
        let _ = self.orchestrator.apply_settings(&settings);
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = settings;
        self
    }

    pub fn with_settings_path(mut self, path: PathBuf) -> Self {
        self.settings_path = Some(path);
        self
    }

    pub(crate) fn on_settings_changed(&self, next: &Settings) {
        self.quota.set_manual_path(PathBuf::from(&next.quota_path));
        let _ = self.scheduler.set_max_concurrency(next.max_concurrency);
        let _ = self
            .scheduler
            .set_sensitive_agents(next.sensitive_agents.clone());
        self.profile_rules.set_hidden(&next.hidden_profiles);
        let _ = self.orchestrator.apply_settings(next);
    }

    /// Agente listo para una tarea: modelo y esfuerzo por tarea > ajustes > el que ya
    /// traía el agente registrado (default en código / sin esfuerzo).
    pub(crate) fn agent_for_task(
        &self,
        kind: AgentKind,
        task_model: Option<&str>,
        task_effort: Option<&str>,
    ) -> Result<Arc<dyn Agent>> {
        let agent = self.agent_with_model(kind, task_model)?;
        self.apply_effort(kind, agent, task_effort)
    }

    fn apply_effort(
        &self,
        kind: AgentKind,
        agent: Arc<dyn Agent>,
        task_effort: Option<&str>,
    ) -> Result<Arc<dyn Agent>> {
        let task_effort = match task_effort.map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => Some(forja_agent::validate_effort(kind, e)?),
            None => None,
        };
        let from_settings = self
            .settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .agent_effort
            .get(&kind.to_string())
            .map(|e| forja_agent::normalize_effort_alias(e))
            .filter(|e| !e.is_empty());
        let explicit_task = task_effort.is_some();
        match task_effort.or(from_settings) {
            None => Ok(agent),
            Some(effort) => match agent.with_effort(&effort) {
                Some(a) => Ok(a),
                None if explicit_task => Err(ForjaError::InvalidInput(format!(
                    "el agente {kind} no admite esfuerzo de razonamiento"
                ))),
                None => Ok(agent),
            },
        }
    }

    /// Validación y alta de una tarea (misma ruta para `POST /tasks` y para las
    /// delegaciones del orquestador): título/prompt, agente, modelo, esfuerzo y
    /// política de privacidad (la comprueba el planificador).
    pub async fn submit_task(&self, req: &CreateTaskRequest) -> Result<RunRecord> {
        let titulo = req.titulo.trim();
        let prompt = req.prompt.trim();
        if titulo.is_empty() {
            return Err(ForjaError::InvalidInput(
                "el título no puede estar vacío".to_string(),
            ));
        }
        if prompt.is_empty() {
            return Err(ForjaError::InvalidInput(
                "el prompt no puede estar vacío".to_string(),
            ));
        }
        let agent =
            self.agent_for_task(req.agent, req.modelo.as_deref(), req.esfuerzo.as_deref())?;
        let task = Task::new(titulo, prompt, req.privacidad);
        self.scheduler.submit(task, agent).await
    }

    fn agent_with_model(
        &self,
        kind: AgentKind,
        task_model: Option<&str>,
    ) -> Result<Arc<dyn Agent>> {
        let base = self
            .get_agent(kind)
            .ok_or_else(|| ForjaError::NotConfigured(format!("agente {kind} no configurado")))?;
        let task_model = match task_model.map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => Some(forja_agent::validate_model(m)?),
            None => None,
        };
        let from_settings = self
            .settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .agent_models
            .get(&kind.to_string())
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let explicit_task = task_model.is_some();
        match task_model.or(from_settings) {
            None => Ok(base),
            Some(model) => match base.with_model(&model) {
                Some(agent) => Ok(agent),
                None if explicit_task => Err(ForjaError::InvalidInput(format!(
                    "el agente {kind} no admite elegir modelo"
                ))),
                None => Ok(base),
            },
        }
    }

    fn get_agent(&self, kind: AgentKind) -> Option<Arc<dyn Agent>> {
        self.agents.get(&kind).cloned()
    }
}
