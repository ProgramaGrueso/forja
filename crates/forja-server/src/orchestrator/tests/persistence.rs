use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;

use crate::conversation_store::{
    load_conversations, write_atomic, PersistedConversation, CONVERSATIONS_FILE,
};

use super::*;

/// Doble que simula `claude` para poder probar `modelo` y `esfuerzo` junto a sesiones.
struct ClaudeSessionFake {
    n: Mutex<u32>,
    resumes: Mutex<Vec<Option<String>>>,
}

impl ClaudeSessionFake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            n: Mutex::new(0),
            resumes: Mutex::new(Vec::new()),
        })
    }
}

impl ChatBackend for ClaudeSessionFake {
    fn agent_name(&self) -> String {
        "claude".to_string()
    }

    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            self.resumes.lock().unwrap().push(resume.clone());
            let sid = resume.unwrap_or_else(|| {
                let mut n = self.n.lock().unwrap();
                *n += 1;
                format!("sess-{}", *n)
            });
            Ok(ChatReply {
                text: format!("resp:{sid}:{}", prompt.lines().last().unwrap_or("")),
                session_id: Some(sid),
            })
        })
    }
}

#[tokio::test]
async fn persistence_roundtrip_two_conversations_and_messages() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(CONVERSATIONS_FILE);

    let orq = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&path);
    orq.say("hola principal", 7777, None).await.unwrap();
    orq.update_options(PRINCIPAL, Some("opus"), Some("high"))
        .unwrap();

    let c2 = orq.create(Some("Proyecto Beta")).unwrap();
    orq.say_in(&c2.id, "primer mensaje beta", 7777, None)
        .await
        .unwrap();
    orq.say_in(&c2.id, "segundo mensaje beta", 7777, None)
        .await
        .unwrap();
    orq.rename(&c2.id, "Proyecto Beta v2");
    orq.flush_persistence();

    assert!(path.is_file(), "debe crear conversations.json");
    let loaded = load_conversations(&path);
    assert_eq!(loaded.len(), 2);

    let p = loaded.iter().find(|c| c.id == PRINCIPAL).unwrap();
    assert_eq!(p.titulo, "Principal");
    assert_eq!(p.session_id.as_deref(), Some("sess-1"));
    assert_eq!(p.modelo.as_deref(), Some("opus"));
    assert_eq!(p.esfuerzo.as_deref(), Some("high"));
    assert_eq!(p.historial.len(), 2);
    assert_eq!(p.historial[0].rol, "usuario");
    assert_eq!(p.historial[0].texto, "hola principal");
    assert_eq!(p.historial[1].rol, "orquestador");

    let b = loaded.iter().find(|c| c.id == c2.id).unwrap();
    assert_eq!(b.titulo, "Proyecto Beta v2");
    assert_eq!(b.session_id.as_deref(), Some("sess-2"));
    assert_eq!(b.historial.len(), 4);
    assert_eq!(b.historial[0].texto, "primer mensaje beta");
    assert_eq!(b.historial[2].texto, "segundo mensaje beta");
    assert!(b.creado > 0 && b.actualizado >= b.creado);
}

#[tokio::test]
async fn persistence_reload_in_new_instance_simulates_restart() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(CONVERSATIONS_FILE);

    let second_id = {
        let orq1 = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&path);
        orq1.say("antes del reinicio", 7777, None).await.unwrap();
        let c2 = orq1.create(Some("Seguimiento")).unwrap();
        orq1.update_options(&c2.id, Some("sonnet"), Some("medium"))
            .unwrap();
        orq1.say_in(&c2.id, "nota inicial", 7777, None)
            .await
            .unwrap();
        c2.id
        // Al hacer drop de `orq1`, el hilo de fondo termina de volcar el último cambio.
    };

    let fake2 = ClaudeSessionFake::new();
    let orq2 = Orchestrator::new(fake2.clone()).with_persistence(&path);

    let list = orq2.list();
    assert_eq!(list.len(), 2);
    let d1 = orq2.get(PRINCIPAL).unwrap();
    assert_eq!(d1.historial.len(), 2);
    assert_eq!(d1.historial[0].texto, "antes del reinicio");
    assert_eq!(orq2.session_of(PRINCIPAL).as_deref(), Some("sess-1"));

    let d2 = orq2.get(&second_id).unwrap();
    assert_eq!(d2.resumen.titulo, "Seguimiento");
    assert_eq!(d2.resumen.modelo.as_deref(), Some("sonnet"));
    assert_eq!(d2.resumen.esfuerzo.as_deref(), Some("medium"));
    assert_eq!(d2.historial.len(), 2);
    assert_eq!(orq2.session_of(&second_id).as_deref(), Some("sess-2"));

    // Al seguir hablando tras el reinicio, se reanuda el session_id conservado.
    orq2.say_in(&second_id, "después del reinicio", 7777, None)
        .await
        .unwrap();
    assert_eq!(
        fake2.resumes.lock().unwrap().as_slice(),
        &[Some("sess-2".to_string())]
    );

    // Crear una nueva conversación tras reiniciar no colisiona con IDs existentes.
    let c3 = orq2.create(Some("Nueva post-reinicio")).unwrap();
    assert_ne!(c3.id, second_id);
    assert_ne!(c3.id, PRINCIPAL);
}

#[tokio::test]
async fn persistence_corrupt_file_starts_with_principal_only() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(CONVERSATIONS_FILE);
    std::fs::write(&path, "{archivo corrupto sin cerrar").unwrap();

    let orq = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&path);
    let list = orq.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, PRINCIPAL);
    assert_eq!(list[0].titulo, "Principal");
    assert_eq!(list[0].mensajes, 0);

    // El servidor sigue operativo y el siguiente cambio regenera un JSON válido.
    orq.say("recuperado", 7777, None).await.unwrap();
    orq.flush_persistence();
    let loaded = load_conversations(&path);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, PRINCIPAL);
    assert_eq!(loaded[0].historial.len(), 2);
}

#[test]
fn persistence_enforces_20_conversations_and_500_messages_on_load() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(CONVERSATIONS_FILE);

    let make_msgs = |n: usize| -> Vec<Message> {
        (0..n)
            .map(|i| Message {
                rol: "usuario".to_string(),
                texto: format!("m-{i}"),
                ts: i as u64,
            })
            .collect()
    };

    // Escribimos directamente JSON con 25 conversaciones (principal la más vieja) y 600 msgs.
    let mut entries = vec![json!({
        "id": PRINCIPAL,
        "titulo": "Principal",
        "session_id": "s-principal",
        "historial": make_msgs(600),
        "creado": 1,
        "actualizado": 1,
    })];
    for i in 1..=24u64 {
        entries.push(json!({
            "id": format!("c{i}-00001"),
            "titulo": format!("Conv {i}"),
            "session_id": null,
            "historial": make_msgs(520),
            "creado": 10,
            "actualizado": 100 + i,
        }));
    }
    let payload = json!({ "version": 1, "conversations": entries });
    std::fs::write(&path, serde_json::to_string(&payload).unwrap()).unwrap();

    let orq = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&path);
    let list = orq.list();
    assert_eq!(list.len(), MAX_CONVERSATIONS);
    assert!(
        orq.get(PRINCIPAL).is_some(),
        "la principal siempre debe existir"
    );
    // c1..=c5 son las 5 más antiguas fuera de principal y deben descartarse.
    for old in 1..=5 {
        assert!(
            orq.get(&format!("c{old}-00001")).is_none(),
            "c{old}-00001 debía descartarse"
        );
    }
    for kept in 6..=24 {
        let d = orq
            .get(&format!("c{kept}-00001"))
            .unwrap_or_else(|| panic!("c{kept}-00001 debía conservarse"));
        assert_eq!(d.historial.len(), MAX_MESSAGES);
        assert_eq!(d.historial.first().unwrap().texto, "m-20");
        assert_eq!(d.historial.last().unwrap().texto, "m-519");
    }
    let p = orq.get(PRINCIPAL).unwrap();
    assert_eq!(p.historial.len(), MAX_MESSAGES);
    assert_eq!(p.historial.first().unwrap().texto, "m-100");
    assert_eq!(p.historial.last().unwrap().texto, "m-599");
}

#[test]
fn persistence_excludes_in_flight_pending_and_auto_turns() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(CONVERSATIONS_FILE);

    let orq1 = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&path);
    // Poblamos estado transitorio en memoria: pending, auto_turns e in_flight.
    assert!(orq1.begin_auto_turn(PRINCIPAL));
    assert!(orq1.begin_auto_turn(PRINCIPAL));
    orq1.defer_reaction(PRINCIPAL, &[("run-1".into(), "Tarea pendiente".into())]);
    {
        let mut g = lock(&orq1.inner);
        let conv = g.map.get_mut(PRINCIPAL).unwrap();
        conv.in_flight = Some(InFlight {
            seq: 99,
            cancel: Arc::new(Notify::new()),
            auto: false,
        });
    }
    // Forzamos un guardado mientras in_flight/pending/auto_turns están activos.
    orq1.rename(PRINCIPAL, "Principal activa");
    orq1.flush_persistence();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("in_flight"));
    assert!(!raw.contains("pending"));
    assert!(!raw.contains("auto_turns"));

    // Incluso si el JSON tuviera campos extra, al cargar quedan vacío/0/None.
    let orq2 = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&path);
    let g = lock(&orq2.inner);
    let conv = g.map.get(PRINCIPAL).unwrap();
    assert!(conv.in_flight.is_none());
    assert!(conv.pending.is_empty());
    assert_eq!(conv.auto_turns, 0);
    assert_eq!(conv.title, "Principal activa");
    drop(g);
    // Puede volver a consumir los MAX_AUTO_TURNS completos tras reiniciar.
    for _ in 0..MAX_AUTO_TURNS {
        assert!(orq2.begin_auto_turn(PRINCIPAL));
    }
    assert!(!orq2.begin_auto_turn(PRINCIPAL));
}

#[tokio::test]
async fn persistence_write_failure_does_not_fail_user_turn() {
    let dir = TempDir::new().unwrap();
    let bad_path = dir.path().join(CONVERSATIONS_FILE);
    std::fs::create_dir(&bad_path).unwrap();

    let orq = Orchestrator::new(ClaudeSessionFake::new()).with_persistence(&bad_path);
    let c = orq.create(Some("Sin error")).unwrap();
    assert!(orq.rename(&c.id, "Renombrada"));
    let reply = orq.say_in(&c.id, "hola", 7777, None).await.unwrap();
    assert!(reply.contains("hola"));
    orq.flush_persistence();
    // Confirmamos que write_atomic directo también respeta límites al escribir.
    let ok_path = dir.path().join("ok.json");
    write_atomic(
        &ok_path,
        &[PersistedConversation {
            id: PRINCIPAL.to_string(),
            titulo: "Principal".to_string(),
            session_id: None,
            historial: Vec::new(),
            creado: 1,
            actualizado: 1,
            modelo: None,
            esfuerzo: None,
        }],
    )
    .unwrap();
    assert_eq!(load_conversations(&ok_path).len(), 1);
}
