use std::sync::{Arc, Mutex};

use super::*;

#[test]
fn history_block_keeps_last_20_exchanges_skips_system_and_clips() {
    assert_eq!(history_block(&[]), "");
    let mut msgs = Vec::new();
    for i in 0..30 {
        msgs.push(Message {
            rol: "usuario".into(),
            texto: format!("pregunta {i}"),
            ts: 0,
        });
        msgs.push(Message {
            rol: "orquestador".into(),
            texto: format!("respuesta {i}"),
            ts: 0,
        });
    }
    msgs.push(Message {
        rol: "sistema".into(),
        texto: "Llamada cancelada con /stop.".into(),
        ts: 0,
    });
    let h = history_block(&msgs);
    assert!(!h.contains("pregunta 9\n") && !h.contains("respuesta 9\n"));
    assert!(h.contains("Usuario: pregunta 10\n"), "{h}");
    assert!(h.contains("Orquestador: respuesta 29"));
    assert!(!h.contains("cancelada"));
    assert_eq!(h.matches("Usuario:").count(), HISTORY_EXCHANGES);

    let long = Message {
        rol: "usuario".into(),
        texto: "x".repeat(5000),
        ts: 0,
    };
    let h = history_block(&[long]);
    assert!(h.contains('…') && h.chars().count() < 1800);
}

#[tokio::test]
async fn first_turn_carries_instructions_then_resumes_session() {
    let fake = Arc::new(Fake(Mutex::new(vec![])));
    let orq = Orchestrator::new(fake.clone());
    assert_eq!(orq.say("hola", 7777, Some("tk")).await.unwrap(), "hecho");
    orq.say("sigue", 7777, Some("tk")).await.unwrap();
    let calls = fake.0.lock().unwrap();
    assert!(calls[0].0.contains("delegar y revisar"));
    assert!(calls[0].0.contains("Bearer tk"));
    // Hermes recibe la pista para que el servidor pueda avisarle al acabar.
    assert!(
        calls[0].0.contains("conversation_id=principal"),
        "{}",
        calls[0].0
    );
    assert!(calls[0].0.ends_with("hola"));
    assert_eq!(calls[0].1, None);
    assert_eq!(calls[1].0, "sigue");
    assert_eq!(calls[1].1.as_deref(), Some("s1"));
    drop(calls);
    orq.reset();
    assert!(orq.current_session().is_none());
}

#[tokio::test]
async fn conversations_have_isolated_sessions_and_history() {
    let fake = SessionFake::new();
    let orq = Orchestrator::new(fake.clone());
    let a = orq.create(Some("A")).unwrap().id;
    let b = orq.create(Some("B")).unwrap().id;
    orq.say_in(&a, "hola a", 1, None).await.unwrap();
    orq.say_in(&b, "hola b", 1, None).await.unwrap();
    orq.say("hola p", 1, None).await.unwrap();
    let (sa, sb, sp) = (
        orq.session_of(&a).unwrap(),
        orq.session_of(&b).unwrap(),
        orq.current_session().unwrap(),
    );
    assert!(sa != sb && sb != sp && sa != sp);
    orq.say_in(&a, "otra", 1, None).await.unwrap();
    assert_eq!(orq.session_of(&a).unwrap(), sa);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.last().unwrap().1.as_deref(), Some(sa.as_str()));
    drop(calls);
    let da = orq.get(&a).unwrap();
    assert_eq!(da.historial.len(), 4);
    assert_eq!(da.historial[0].rol, "usuario");
    assert_eq!(da.historial[1].rol, "orquestador");
    assert_eq!(orq.get(&b).unwrap().historial.len(), 2);
    assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 2);
}

#[tokio::test]
async fn two_conversations_talk_at_the_same_time() {
    let orq = Orchestrator::new(Arc::new(BarrierFake(tokio::sync::Barrier::new(2))));
    let a = orq.create(None).unwrap().id;
    let b = orq.create(None).unwrap().id;
    let (o1, o2) = (orq.clone(), orq.clone());
    let work =
        async move { tokio::join!(o1.say_in(&a, "uno", 1, None), o2.say_in(&b, "dos", 1, None)) };
    // Si un lock se mantuviera durante la llamada, el Barrier nunca se liberaría.
    let (ra, rb) = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .expect("las dos conversaciones deben avanzar a la vez");
    assert_eq!(ra.unwrap(), "par");
    assert_eq!(rb.unwrap(), "par");
}

#[tokio::test]
async fn same_conversation_rejects_overlapping_calls_and_stop_cancels() {
    let orq = Orchestrator::new(Arc::new(HangFake));
    let id = orq.create(None).unwrap().id;
    let (o, i) = (orq.clone(), id.clone());
    let task = tokio::spawn(async move { o.say_in(&i, "cuelga", 1, None).await });
    while !orq.get(&id).unwrap().resumen.ocupada {
        tokio::task::yield_now().await;
    }
    assert_eq!(orq.say_in(&id, "otra", 1, None).await, Err(ChatError::Busy));
    assert!(orq.stop(&id));
    assert_eq!(task.await.unwrap(), Err(ChatError::Cancelled));
    let d = orq.get(&id).unwrap();
    assert!(!d.resumen.ocupada);
    assert_eq!(d.historial.last().unwrap().rol, "sistema");
    assert!(!orq.stop(&id), "ya no hay nada en curso");
}

#[tokio::test]
async fn delete_removes_conversation_but_principal_is_only_cleared() {
    let orq = Orchestrator::new(SessionFake::new());
    let a = orq.create(None).unwrap().id;
    orq.say_in(&a, "hola", 1, None).await.unwrap();
    assert!(orq.delete(&a));
    assert!(orq.get(&a).is_none());
    assert!(!orq.delete(&a));
    assert_eq!(orq.say_in(&a, "x", 1, None).await, Err(ChatError::NotFound));
    orq.say("p", 1, None).await.unwrap();
    assert!(orq.delete(PRINCIPAL));
    assert!(orq.current_session().is_none());
    assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 0);
}

#[tokio::test]
async fn limits_on_conversations_and_messages() {
    let orq = Orchestrator::new(SessionFake::new());
    // La principal ya cuenta.
    for _ in 1..MAX_CONVERSATIONS {
        orq.create(None).unwrap();
    }
    assert_eq!(orq.create(None), Err(CreateError::LimitReached));
    assert_eq!(orq.list().len(), MAX_CONVERSATIONS);
    for i in 0..(MAX_MESSAGES / 2 + 10) {
        orq.say(&format!("m{i}"), 1, None).await.unwrap();
    }
    let d = orq.get(PRINCIPAL).unwrap();
    assert_eq!(d.historial.len(), MAX_MESSAGES);
    assert_eq!(d.resumen.mensajes, MAX_MESSAGES);
    assert!(d.historial.last().unwrap().texto == "ok");
    // Se descartaron los más antiguos.
    assert!(d.historial.iter().all(|m| m.texto != "m0"));
}

#[tokio::test]
async fn title_is_automatic_from_first_message_and_rename_wins() {
    let orq = Orchestrator::new(SessionFake::new());
    let a = orq.create(None).unwrap().id;
    assert_eq!(orq.get(&a).unwrap().resumen.titulo, "");
    orq.say_in(&a, "uno dos tres cuatro cinco seis siete ocho", 1, None)
        .await
        .unwrap();
    assert_eq!(
        orq.get(&a).unwrap().resumen.titulo,
        "uno dos tres cuatro cinco seis"
    );
    orq.say_in(&a, "otro mensaje distinto", 1, None)
        .await
        .unwrap();
    assert_eq!(
        orq.get(&a).unwrap().resumen.titulo,
        "uno dos tres cuatro cinco seis"
    );
    assert!(orq.rename(&a, "  Mi título  "));
    assert_eq!(orq.get(&a).unwrap().resumen.titulo, "Mi título");
    assert!(!orq.rename("nope", "x"));
    let named = orq.create(Some("Fijo")).unwrap().id;
    orq.say_in(&named, "hola", 1, None).await.unwrap();
    assert_eq!(orq.get(&named).unwrap().resumen.titulo, "Fijo");
}
