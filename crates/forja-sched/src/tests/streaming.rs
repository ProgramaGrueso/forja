use std::sync::Arc;
use std::time::Duration;

use forja_agent::MockAgent;
use forja_core::{ForjaError, RunStatus};

use super::{new_scheduler, open_task, ScriptAgent};

#[tokio::test]
async fn output_streams_live_before_run_finishes() {
    let (sched, _a, _b) = new_scheduler(2);
    let agent = ScriptAgent::arc(&[("FAKE_STREAM_LINES", "4"), ("FAKE_STREAM_SLEEP", "0.15")]);
    let run = sched
        .submit(open_task("live"), agent)
        .await
        .expect("submit");

    let mut sub = sched.subscribe_output(&run.id, 0).await.expect("subscribe");
    let first = tokio::time::timeout(Duration::from_secs(5), sub.next_batch())
        .await
        .expect("sin timeout")
        .expect("primer lote");
    assert_eq!(first[0].text, "linea 1\n");
    assert_eq!(first[0].stream, "stdout");
    assert_eq!(first[0].seq, 0);
    let status = sched.get_run(&run.id).await.expect("run").status;
    assert_eq!(status, RunStatus::Running, "debe seguir corriendo");

    let mut texts: Vec<String> = first.into_iter().map(|c| c.text).collect();
    while let Some(batch) = sub.next_batch().await {
        texts.extend(batch.into_iter().map(|c| c.text));
    }
    assert_eq!(texts, ["linea 1\n", "linea 2\n", "linea 3\n", "linea 4\n"]);
    let done = sched.wait_run(&run.id).await.expect("wait");
    assert_eq!(done.status, RunStatus::Done);
}

#[tokio::test]
async fn output_buffer_is_capped_and_since_reconnects() {
    let (sched, _a, _b) = new_scheduler(2);
    sched.set_output_limits(5, 1024 * 1024);
    let agent = ScriptAgent::arc(&[("FAKE_STREAM_LINES", "20"), ("FAKE_STREAM_SLEEP", "0")]);
    let run = sched.submit(open_task("cap"), agent).await.expect("submit");
    sched.wait_run(&run.id).await.expect("wait");

    let page = sched.output_since(&run.id, 0).await.expect("page");
    assert_eq!(page.chunks.len(), 5);
    assert_eq!(page.chunks[0].seq, 15);
    assert_eq!(page.chunks[0].text, "linea 16\n");
    assert_eq!(page.next_seq, 20);
    assert!(page.truncated);
    assert!(page.finished);

    let page = sched.output_since(&run.id, 18).await.expect("page");
    assert_eq!(
        page.chunks.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [18, 19]
    );
    assert!(!page.truncated);
    let page = sched.output_since(&run.id, 20).await.expect("page");
    assert!(page.chunks.is_empty());
    assert_eq!(page.next_seq, 20);
}

#[tokio::test]
async fn output_byte_cap_drops_oldest() {
    let (sched, _a, _b) = new_scheduler(1);
    sched.set_output_limits(2000, 40);
    let agent = ScriptAgent::arc(&[("FAKE_STREAM_LINES", "30"), ("FAKE_STREAM_SLEEP", "0")]);
    let run = sched
        .submit(open_task("bytes"), agent)
        .await
        .expect("submit");
    sched.wait_run(&run.id).await.expect("wait");
    let page = sched.output_since(&run.id, 0).await.expect("page");
    let bytes: usize = page.chunks.iter().map(|c| c.text.len()).sum();
    assert!(bytes <= 40, "bytes retenidos: {bytes}");
    assert_eq!(page.chunks.last().expect("último").text, "linea 30\n");
    assert!(page.truncated);
}

#[tokio::test]
async fn two_runs_stream_in_parallel_without_mixing() {
    let (sched, _a, _b) = new_scheduler(2);
    let mk = |tag: &str| {
        ScriptAgent::arc(&[
            ("FAKE_STREAM_LINES", "5"),
            ("FAKE_STREAM_SLEEP", "0.2"),
            ("FAKE_STREAM_TAG", tag),
        ])
    };
    let a = sched.submit(open_task("A"), mk("alfa")).await.expect("a");
    let b = sched.submit(open_task("B"), mk("beta")).await.expect("b");

    // Ambos emiten algo mientras ninguno ha terminado.
    let mut overlapped = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let pa = sched.output_since(&a.id, 0).await.expect("pa");
        let pb = sched.output_since(&b.id, 0).await.expect("pb");
        if !pa.chunks.is_empty() && !pb.chunks.is_empty() && !pa.finished && !pb.finished {
            overlapped = true;
            break;
        }
    }
    assert!(overlapped, "los dos runs debieron transmitir a la vez");

    sched.wait_all().await;
    for (id, tag) in [(&a.id, "alfa"), (&b.id, "beta")] {
        let page = sched.output_since(id, 0).await.expect("page");
        let texts: Vec<String> = page.chunks.iter().map(|c| c.text.clone()).collect();
        let expected: Vec<String> = (1..=5).map(|i| format!("{tag} {i}\n")).collect();
        assert_eq!(texts, expected);
    }
}

#[tokio::test]
async fn max_concurrency_still_applies_to_streaming_runs() {
    let (sched, _a, _b) = new_scheduler(1);
    let mk = |tag: &str| {
        ScriptAgent::arc(&[
            ("FAKE_STREAM_LINES", "3"),
            ("FAKE_STREAM_SLEEP", "0.15"),
            ("FAKE_STREAM_TAG", tag),
        ])
    };
    let a = sched.submit(open_task("A"), mk("uno")).await.expect("a");
    let b = sched.submit(open_task("B"), mk("dos")).await.expect("b");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pb = sched.output_since(&b.id, 0).await.expect("pb");
    assert!(pb.chunks.is_empty(), "B no debe arrancar con límite 1");
    assert!(!sched
        .output_since(&a.id, 0)
        .await
        .expect("pa")
        .chunks
        .is_empty());
    sched.wait_all().await;
}

#[tokio::test]
async fn finished_run_replays_buffer_and_closes() {
    let (sched, _a, _b) = new_scheduler(2);
    let run = sched
        .submit(
            open_task("fin"),
            ScriptAgent::arc(&[("FAKE_STREAM_LINES", "2"), ("FAKE_STREAM_SLEEP", "0")]),
        )
        .await
        .expect("submit");
    sched.wait_run(&run.id).await.expect("wait");

    let mut sub = sched.subscribe_output(&run.id, 0).await.expect("sub");
    let batch = sub.next_batch().await.expect("backlog");
    assert_eq!(batch.len(), 2);
    assert!(sub.next_batch().await.is_none(), "debe cerrar");
}

#[tokio::test]
async fn non_streaming_agent_emits_output_at_end() {
    let (sched, _a, _b) = new_scheduler(2);
    let run = sched
        .submit(
            open_task("mock"),
            Arc::new(MockAgent::success("hola\nmundo")),
        )
        .await
        .expect("submit");
    sched.wait_run(&run.id).await.expect("wait");
    let page = sched.output_since(&run.id, 0).await.expect("page");
    let all: String = page.chunks.iter().map(|c| c.text.as_str()).collect();
    assert!(all.contains("hola\nmundo"), "salida: {all:?}");
    assert!(page.finished);
}

#[tokio::test]
async fn output_is_redacted_before_emission() {
    let (sched, _a, _b) = new_scheduler(2);
    let run = sched
        .submit(
            open_task("secret"),
            ScriptAgent::arc(&[
                ("FAKE_STREAM_LINES", "1"),
                ("FAKE_STREAM_SLEEP", "0"),
                ("FAKE_STREAM_SECRET", "sk-live1234567890abcdef"),
            ]),
        )
        .await
        .expect("submit");
    sched.wait_run(&run.id).await.expect("wait");
    let page = sched.output_since(&run.id, 0).await.expect("page");
    let all: String = page.chunks.iter().map(|c| c.text.as_str()).collect();
    assert!(all.contains("clave ***"), "salida: {all:?}");
    assert!(!all.contains("sk-live"));
}

#[tokio::test]
async fn output_unknown_or_invalid_id_is_not_found() {
    let (sched, _a, _b) = new_scheduler(1);
    for id in ["no-existe", "../etc/passwd", "", "a b"] {
        let err = sched.output_since(id, 0).await.expect_err("debe fallar");
        assert!(matches!(err, ForjaError::NotFound(_)), "{id}: {err:?}");
        assert!(sched.subscribe_output(id, 0).await.is_err());
    }
}
