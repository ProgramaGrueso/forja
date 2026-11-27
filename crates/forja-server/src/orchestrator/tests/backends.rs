use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use super::*;

#[test]
fn resolve_profile_priority_settings_env_default() {
    assert_eq!(resolve_profile(Some("claudia"), Some("yuuki")), "claudia");
    assert_eq!(resolve_profile(None, Some("yuuki")), "yuuki");
    assert_eq!(resolve_profile(Some("  "), Some("yuuki")), "yuuki");
    assert_eq!(resolve_profile(None, None), "default");
    assert_eq!(resolve_profile(Some(""), Some(" ")), "default");
}

#[tokio::test]
async fn apply_profile_switches_hot_and_resets_session_only_on_change() {
    let (_home, rules) = fake_rules(&["claudia"]);
    let backend = Arc::new(Profiled(RwLock::new("default".into())));
    let orq = Orchestrator::new(backend.clone()).with_rules(rules);
    orq.say("hola", 7777, None).await.unwrap();
    assert!(orq.current_session().is_some());

    // Mismo perfil efectivo: la sesión se conserva (si el env no interfiere).
    if std::env::var(PROFILE_ENV).is_err() {
        assert_eq!(orq.apply_profile(None).unwrap(), "default");
        assert!(orq.current_session().is_some());
    }

    assert_eq!(orq.apply_profile(Some("Claudia")).unwrap(), "claudia");
    assert_eq!(backend.profile().as_deref(), Some("claudia"));
    assert!(orq.current_session().is_none());
}

#[test]
fn apply_profile_rejects_hidden_and_unknown_profiles() {
    let (_home, rules) = fake_rules(&["claudia", "privado"]);
    rules.set_hidden(&["privado".to_string()]);
    let backend = Arc::new(Profiled(RwLock::new("default".into())));
    let orq = Orchestrator::new(backend.clone()).with_rules(rules);
    for bad in ["privado", "otro"] {
        assert!(orq.apply_profile(Some(bad)).is_err(), "{bad}");
    }
    assert_eq!(backend.profile().as_deref(), Some("default"));
    assert!(orq.apply_profile(Some("claudia")).is_ok());
}

#[test]
fn check_settings_uses_discovery_and_the_hidden_of_the_same_settings() {
    let (_home, rules) = fake_rules(&["claudia", "privado"]);
    let orq = Orchestrator::new(Arc::new(Fake(Mutex::new(vec![])))).with_rules(rules);
    let mut s = base_settings();
    s.orchestrator_profile = Some("claudia".into());
    assert!(orq.check_settings(&s).is_ok());
    s.orchestrator_profile = Some("fantasma".into());
    assert!(orq.check_settings(&s).is_err());
    s.orchestrator_profile = Some("privado".into());
    s.hidden_profiles = vec!["privado".into()];
    assert!(orq.check_settings(&s).is_err());
    assert_eq!(orq.profiles(), vec!["default", "claudia", "privado"]);
}

#[test]
fn resolve_agent_priority_settings_env_auto() {
    let all = only(&["claude", "opencode", "agy", "local", "hermes"]);
    let c = resolve_agent(Some("agy"), Some("local"), &all);
    assert_eq!((c.agent.as_str(), c.source), ("agy", "settings"));
    let c = resolve_agent(Some("  "), Some("Local"), &all);
    assert_eq!((c.agent.as_str(), c.source), ("local", "env"));
    // Valores inválidos se ignoran y se cae a automático.
    let c = resolve_agent(Some("mock"), Some("gpt"), &all);
    assert_eq!((c.agent.as_str(), c.source), ("claude", "auto"));
    // Orden automático: claude, opencode, agy, local, hermes.
    for (installed, want) in [
        (&["hermes", "local", "agy", "opencode"][..], "opencode"),
        (&["hermes", "local", "agy"][..], "agy"),
        (&["hermes", "local"][..], "local"),
        (&["hermes"][..], "hermes"),
        (&[][..], "hermes"),
    ] {
        let probe = move |a: &str| installed.contains(&a);
        assert_eq!(resolve_agent(None, None, &probe).agent, want);
    }
}

#[tokio::test]
async fn cli_backend_runs_each_agent_one_shot_with_resolved_model() {
    let models: BTreeMap<String, String> = [("claude".to_string(), "opus".to_string())].into();
    for (kind, model) in [
        ("claude", "opus"),
        ("opencode", "opencode-go/deepseek-v4.1-flash"),
        ("agy", "gemini-4-neon-medium"),
    ] {
        let b = CliBackend::new(kind, models.clone()).with_program(fake_script());
        assert!(b.stateless());
        assert_eq!(b.agent_name(), kind);
        let r = b
            .chat("hola \"mundo\"\nlinea 2".to_string(), None)
            .await
            .unwrap();
        assert!(
            r.text.contains(&format!("ECO[modelo={model}]")),
            "{kind}: {}",
            r.text
        );
        assert!(
            r.text.contains("hola \"mundo\"\nlinea 2"),
            "{kind}: {}",
            r.text
        );
        assert_eq!(r.session_id, None);
    }
}

#[tokio::test]
async fn cli_backend_hot_models_missing_program_and_failures() {
    let b = CliBackend::new("claude", BTreeMap::new()).with_program(fake_script());
    let r = b.chat("a".into(), None).await.unwrap();
    assert!(r.text.contains("ECO[modelo=sonnet]"), "{}", r.text);
    b.set_models(&[("claude".to_string(), "haiku".to_string())].into());
    let r = b.chat("a".into(), None).await.unwrap();
    assert!(r.text.contains("ECO[modelo=haiku]"), "{}", r.text);

    let missing = CliBackend::new("opencode", BTreeMap::new())
        .with_program("/no/existe/forja-falso-orquestador");
    assert!(missing.chat("a".into(), None).await.is_err());

    // hermes y mock no son orquestadores one-shot.
    assert!(CliBackend::new("hermes", BTreeMap::new())
        .make_agent()
        .is_err());
    assert!(CliBackend::new("nope", BTreeMap::new())
        .make_agent()
        .is_err());
    // local resuelve su modelo (alias moe) sin ejecutar nada.
    let local = CliBackend::new("local", BTreeMap::new())
        .make_agent()
        .unwrap();
    assert_eq!(local.model().as_deref(), Some("moe"));
}

#[tokio::test]
async fn stateless_backend_gets_history_each_turn_and_never_resumes() {
    let (_home, rules) = fake_rules(&[]);
    let factory = BackendFactory::system(rules)
        .with_probe(only(&["claude"]))
        .with_program("claude", fake_script());
    let orq = Orchestrator::from_factory(factory);
    if std::env::var(AGENT_ENV).is_err() {
        assert_eq!(orq.agent(), "claude");
    }
    orq.apply_settings(&{
        let mut s = base_settings();
        s.orchestrator_agent = Some("claude".into());
        s
    })
    .unwrap();
    let r1 = orq.say("primera pregunta", 7777, Some("tk")).await.unwrap();
    assert!(
        r1.contains("delegar y revisar"),
        "lleva instrucciones: {r1}"
    );
    assert!(r1.contains("Bearer tk"));
    assert!(
        !r1.contains("Historial reciente"),
        "sin historial en el turno 1"
    );
    let r2 = orq.say("segunda pregunta", 7777, None).await.unwrap();
    assert!(
        r2.contains("delegar y revisar"),
        "cada llamada lleva instrucciones"
    );
    assert!(r2.contains("Historial reciente"), "{r2}");
    assert!(r2.contains("Usuario: primera pregunta"));
    assert!(r2.contains("Orquestador: ECO[modelo=sonnet]"));
    assert!(r2.contains("Mensaje del usuario:\nsegunda pregunta"));
    // El mensaje actual no se duplica dentro del historial.
    assert_eq!(r2.matches("Usuario: segunda pregunta").count(), 0);
    assert!(orq.current_session().is_none());
}

#[tokio::test]
async fn apply_settings_switches_agent_hot_and_resets_sessions() {
    let (_home, rules) = fake_rules(&["claudia"]);
    let factory = BackendFactory::system(rules)
        .with_probe(only(&["opencode", "hermes"]))
        .with_program("opencode", fake_script());
    let orq = Orchestrator::from_factory(factory);
    let mut s = base_settings();
    s.orchestrator_agent = Some("opencode".into());
    orq.apply_settings(&s).unwrap();
    assert_eq!(orq.agent(), "opencode");
    assert_eq!(orq.profile(), None);
    assert_eq!(orq.agents_available(), vec!["opencode", "hermes"]);
    let r = orq.say("hola", 7777, None).await.unwrap();
    assert!(
        r.contains("ECO[modelo=opencode-go/deepseek-v4.1-flash]"),
        "{r}"
    );

    // El modelo de los ajustes se aplica en caliente.
    s.agent_models.insert("opencode".into(), "oc/otro".into());
    orq.apply_settings(&s).unwrap();
    let r = orq.say("de nuevo", 7777, None).await.unwrap();
    assert!(r.contains("ECO[modelo=oc/otro]"), "{r}");

    // Cambiar a hermes aplica el perfil y reinicia sesiones.
    s.orchestrator_agent = Some("hermes".into());
    s.orchestrator_profile = Some("claudia".into());
    orq.apply_settings(&s).unwrap();
    assert_eq!(orq.agent(), "hermes");
    assert_eq!(orq.profile().as_deref(), Some("claudia"));

    // Un perfil que no existe falla y no cambia el perfil activo.
    s.orchestrator_profile = Some("fantasma".into());
    assert!(orq.apply_settings(&s).is_err());
    assert_eq!(orq.profile().as_deref(), Some("claudia"));

    // Quitar el ajuste de agente vuelve a automático (opencode instalado).
    s.orchestrator_agent = None;
    s.orchestrator_profile = None;
    if std::env::var(AGENT_ENV).is_err() {
        orq.apply_settings(&s).unwrap();
        assert_eq!(orq.agent(), "opencode");
        assert_eq!(orq.agent_source(None), "auto");
    }
}

#[tokio::test]
async fn hidden_profile_in_settings_is_rejected_by_hermes_backend_at_chat_time() {
    let (_home, rules) = fake_rules(&["privado"]);
    // Programa inexistente: si el rechazo no ocurriera antes, fallaría distinto.
    let backend = HermesBackend::new(rules.clone()).with_program("/no/existe/hermes-falso");
    backend.set_profile("privado");
    rules.set_hidden(&["privado".to_string()]);
    let err = backend.chat("hola".into(), None).await.unwrap_err();
    assert!(err.contains("oculto"), "{err}");
}

#[test]
fn parses_session_id_from_stderr() {
    assert_eq!(
        parse_session_id("\nsession_id: abc-1\n").as_deref(),
        Some("abc-1")
    );
    assert_eq!(parse_session_id("nada"), None);
}

#[test]
fn argv_has_resume_only_when_given() {
    let f = Path::new("/tmp/q.txt");
    let a = build_argv("claudia", f, None);
    assert_eq!(a[..3], ["hermes", "-p", "claudia"]);
    assert!(!a.contains(&"--resume".to_string()));
    let b = build_argv("claudia", f, Some("s9"));
    assert_eq!(b[b.len() - 2..], ["--resume", "s9"]);
}
