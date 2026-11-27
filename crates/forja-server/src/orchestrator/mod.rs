//! Orquestador conversacional: un agente (Hermes, claude, opencode, agy o local)
//! que habla con el usuario, delega tareas en Forja y revisa sus resultados. El
//! panel lo usa por texto o por voz (Web Speech API en el navegador).
//!
//! Hermes mantiene sesión (`--resume`); los demás CLIs se invocan one-shot y no
//! recuerdan nada, así que el [`Orchestrator`] les reenvía el historial reciente
//! en cada llamada ([`CliBackend`], [`history_block`]).

mod backends;
mod chat;
mod commands;
mod conversations;
mod delegation;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use forja_agent::hermes::ProfileRules;
use tokio::sync::Notify;

use crate::conversation_store::ConversationPersistence;
use crate::settings::Settings;

pub use backends::{
    agent_installed, build_argv, build_argv_with, installed_agents, parse_session_id,
    resolve_agent, resolve_profile, AgentChoice, BackendFactory, ChatBackend, ChatFuture, ChatOpts,
    ChatReply, CliBackend, HermesBackend, AGENT_ENV, AUTO_ORDER, ORCHESTRATOR_AGENTS, PROFILE_ENV,
};
pub use chat::{chat, system_prompt, ChatRequest};
pub use commands::{help_text, parse_command, Command, COMMAND_HELP};
pub use conversations::{
    auto_title, create_conversation, delete_conversation, get_conversation, history_block,
    list_conversations, reset, ChatError, ConversationDetail, ConversationSummary, CreateError,
    CreateRequest, Message, ResetRequest, HISTORY_EXCHANGES, MAX_CONVERSATIONS, MAX_MESSAGES,
    PRINCIPAL,
};
pub use delegation::{AUTO_IDLE_WAIT, MAX_AUTO_TURNS};

use backends::{env_agent, env_profile};
use conversations::{Conversation, Inner};
use delegation::Reactions;

#[derive(Clone)]
pub struct Orchestrator {
    backend: Arc<RwLock<Arc<dyn ChatBackend>>>,
    /// Con fábrica, el agente se puede cambiar en caliente; sin ella el backend es fijo.
    factory: Option<BackendFactory>,
    rules: ProfileRules,
    inner: Arc<Mutex<Inner>>,
    reactions: Arc<Mutex<Reactions>>,
    persistence: Option<Arc<ConversationPersistence>>,
}

impl Orchestrator {
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        let mut map = HashMap::new();
        map.insert(PRINCIPAL.to_string(), Conversation::new("Principal"));
        Self {
            backend: Arc::new(RwLock::new(backend)),
            factory: None,
            rules: ProfileRules::new(),
            inner: Arc::new(Mutex::new(Inner {
                map,
                counter: 0,
                seq: 0,
                idle: Arc::new(Notify::new()),
            })),
            reactions: Arc::new(Mutex::new(Reactions::default())),
            persistence: None,
        }
    }

    /// Reglas de perfiles (descubrimiento y ocultos) para validar en `apply_profile`.
    pub fn with_rules(mut self, rules: ProfileRules) -> Self {
        self.rules = rules.clone();
        let agent = self.agent();
        if let Some(f) = self.factory.as_mut() {
            f.rules = rules;
            let fresh = f.build(&agent, &BTreeMap::new());
            *self.backend.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        }
        self
    }

    /// Orquestador con agente intercambiable. El agente inicial sale de
    /// `FORJA_ORCHESTRATOR_AGENT` o de la detección automática.
    pub fn from_factory(factory: BackendFactory) -> Self {
        let choice = resolve_agent(None, env_agent().as_deref(), &*factory.probe);
        let backend = factory.build(&choice.agent, &BTreeMap::new());
        let mut orq = Self::new(backend).with_rules(factory.rules.clone());
        orq.factory = Some(factory);
        orq
    }

    /// Único punto donde se decide el backend inicial.
    pub fn from_env() -> Self {
        Self::from_factory(BackendFactory::system(ProfileRules::new()))
    }

    pub fn rules(&self) -> &ProfileRules {
        &self.rules
    }

    fn backend(&self) -> Arc<dyn ChatBackend> {
        self.backend
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Agente activo (`hermes`, `claude`, ...).
    pub fn agent(&self) -> String {
        self.backend().agent_name()
    }

    /// Agentes instalados que pueden orquestar (en el orden automático).
    pub fn agents_available(&self) -> Vec<String> {
        match &self.factory {
            Some(f) => installed_agents(&*f.probe),
            None => installed_agents(&agent_installed),
        }
    }

    /// Origen del agente activo según `setting`: `settings` | `env` | `auto`.
    pub fn agent_source(&self, setting: Option<&str>) -> &'static str {
        let probe: &dyn Fn(&str) -> bool = match &self.factory {
            Some(f) => &*f.probe,
            None => &agent_installed,
        };
        resolve_agent(setting, env_agent().as_deref(), probe).source
    }

    /// Perfiles de Hermes elegibles (descubiertos menos ocultos).
    pub fn profiles(&self) -> Vec<String> {
        self.rules.discover()
    }

    /// Comprueba, SIN aplicar nada, que el perfil de los ajustes existe y no está
    /// oculto (con los ocultos de los propios ajustes incluidos).
    pub fn check_settings(&self, s: &Settings) -> Result<(), String> {
        if let Some(profile) = &s.orchestrator_profile {
            self.rules
                .validate_with_extra_hidden(profile, &s.hidden_profiles)
                .map_err(|e| format!("orchestrator_profile: {e}"))?;
        }
        Ok(())
    }

    /// Aplica en caliente ocultos, agente, modelos y perfil. Si cambia el agente o
    /// el perfil se reinician las sesiones. Con ajustes ya validados por
    /// [`Self::check_settings`] solo falla si el perfil desapareció del disco.
    pub fn apply_settings(&self, s: &Settings) -> Result<(), String> {
        self.rules.set_hidden(&s.hidden_profiles);
        if let Some(f) = &self.factory {
            let choice = resolve_agent(
                s.orchestrator_agent.as_deref(),
                env_agent().as_deref(),
                &*f.probe,
            );
            if self.agent() != choice.agent {
                let fresh = f.build(&choice.agent, &s.agent_models);
                *self.backend.write().unwrap_or_else(|e| e.into_inner()) = fresh;
                self.reset_all_sessions();
                // Modelo y esfuerzo por conversación pertenecen a la familia anterior.
                self.clear_conversation_options();
            }
        }
        self.backend().set_models(&s.agent_models);
        self.backend().set_efforts(&s.agent_effort);
        if self.backend().profile().is_some() {
            self.apply_profile(s.orchestrator_profile.as_deref())?;
        }
        Ok(())
    }

    /// Aplica el perfil de los ajustes (con env y `default` como respaldo). Si el
    /// perfil cambia, se reinicia la sesión. Devuelve el perfil efectivo.
    pub fn apply_profile(&self, setting: Option<&str>) -> Result<String, String> {
        let wanted = resolve_profile(setting, env_profile().as_deref());
        let profile = self.rules.validate(&wanted).map_err(|e| e.to_string())?;
        let backend = self.backend();
        if backend.profile().as_deref() != Some(profile.as_str()) {
            backend.set_profile(&profile);
            self.reset_all_sessions();
        }
        Ok(profile)
    }

    pub fn profile(&self) -> Option<String> {
        self.backend().profile()
    }
}
