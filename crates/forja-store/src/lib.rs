use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use forja_core::RunRecord;

#[derive(Debug, Clone)]
pub struct JsonlStore {
    dir: PathBuf,
    file_path: PathBuf,
}

fn ends_with_newline(file: &File) -> std::io::Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut last = [0u8; 1];
        file.read_exact_at(&mut last, len - 1)?;
        Ok(last[0] == b'\n')
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    }
}

/// Líneas ilegibles ya avisadas (para no repetir el aviso en cada lectura).
static WARNED_BAD_LINES: AtomicUsize = AtomicUsize::new(0);

impl JsonlStore {
    pub fn new(dir: impl Into<PathBuf>) -> forja_core::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let file_path = dir.join("runs.jsonl");
        Ok(Self { dir, file_path })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// Añade un registro como UNA línea completa.
    ///
    /// Varios procesos (`forja serve` y `forja run`) comparten el archivo: la
    /// línea se escribe con una sola llamada en modo append y bajo un bloqueo
    /// exclusivo entre procesos (`flock`), así dos escrituras nunca se mezclan.
    pub fn append_run(&self, record: &RunRecord) -> forja_core::Result<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.file_path)?;
        file.lock()?;
        // Si una escritura anterior quedó cortada (sin `\n` final), se cierra
        // esa línea para no estropear también la nueva.
        if !ends_with_newline(&file)? {
            line.insert(0, '\n');
        }
        let res = file.write_all(line.as_bytes());
        let _ = file.unlock();
        res?;
        Ok(())
    }

    /// Lee todos los registros (el último estado de cada id gana).
    ///
    /// Tolera líneas ilegibles (escritura interrumpida, bytes no UTF-8, JSON
    /// roto): se saltan con un aviso por stderr en vez de impedir el arranque.
    pub fn list_runs(&self) -> forja_core::Result<Vec<RunRecord>> {
        let file = match File::open(&self.file_path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        // Bloqueo compartido: no leer una línea a medio escribir por otro proceso.
        file.lock_shared()?;
        let mut reader = BufReader::new(&file);
        let mut runs: Vec<RunRecord> = Vec::new();
        let mut indices: HashMap<String, usize> = HashMap::new();
        let mut bad = 0usize;
        let mut buf = Vec::new();

        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            let Ok(line) = std::str::from_utf8(&buf) else {
                bad += 1;
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<RunRecord>(line) else {
                bad += 1;
                continue;
            };
            if let Some(&idx) = indices.get(&record.id) {
                runs[idx] = record;
            } else {
                let idx = runs.len();
                indices.insert(record.id.clone(), idx);
                runs.push(record);
            }
        }
        drop(reader);
        let _ = file.unlock();

        if bad > WARNED_BAD_LINES.load(Ordering::Relaxed) {
            WARNED_BAD_LINES.store(bad, Ordering::Relaxed);
            eprintln!(
                "aviso: {} contiene {bad} línea(s) ilegibles; se ignoran",
                self.file_path.display()
            );
        }
        Ok(runs)
    }

    pub fn get_run(&self, id: &str) -> forja_core::Result<Option<RunRecord>> {
        Ok(self.list_runs()?.into_iter().find(|r| r.id == id))
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use forja_core::{AgentKind, Privacy, RunRecord, RunStatus, Task};
    use tempfile::tempdir;

    use super::JsonlStore;

    fn sample_record(id: &str, status: RunStatus, updated_at_ms: u64) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            task: Task {
                id: format!("task-{id}"),
                titulo: format!("Tarea {id}"),
                prompt: "Ejecutar prueba".to_string(),
                privacidad: Privacy::Open,
            },
            agent: AgentKind::Mock,
            status,
            branch: Some(format!("forja/{id}")),
            worktree: Some(format!("/tmp/wt/{id}")),
            attempts: 1,
            model: None,
            effort: None,
            output: None,
            error: None,
            repo: None,
            owner: None,
            prompt_redacted: false,
            created_at_ms: 1000,
            updated_at_ms,
        }
    }

    #[test]
    fn empty_store_returns_empty_list_and_none() {
        let dir = tempdir().expect("create tempdir");
        let store = JsonlStore::new(dir.path()).expect("init store");

        assert_eq!(store.dir(), dir.path());
        assert_eq!(store.file_path(), dir.path().join("runs.jsonl"));
        assert!(store.list_runs().expect("list runs").is_empty());
        assert!(store.get_run("run-1").expect("get run").is_none());
    }

    #[test]
    fn append_list_and_get_multiple_runs() {
        let dir = tempdir().expect("create tempdir");
        let store = JsonlStore::new(dir.path()).expect("init store");

        let r1 = sample_record("run-1", RunStatus::Pending, 1000);
        let r2 = sample_record("run-2", RunStatus::Done, 2000);

        store.append_run(&r1).expect("append r1");
        store.append_run(&r2).expect("append r2");

        let runs = store.list_runs().expect("list runs");
        assert_eq!(runs, vec![r1.clone(), r2.clone()]);
        assert_eq!(store.get_run("run-1").expect("get run-1"), Some(r1));
        assert_eq!(store.get_run("run-2").expect("get run-2"), Some(r2));
        assert_eq!(store.get_run("run-3").expect("get run-3"), None);
    }

    #[test]
    fn append_only_updates_preserve_latest_state_and_order() {
        let dir = tempdir().expect("create tempdir");
        let store = JsonlStore::new(dir.path()).expect("init store");

        let r1_pending = sample_record("run-1", RunStatus::Pending, 1000);
        let r2_pending = sample_record("run-2", RunStatus::Pending, 1100);
        let r1_running = sample_record("run-1", RunStatus::Running, 1500);
        let mut r1_done = sample_record("run-1", RunStatus::Done, 2000);
        r1_done.output = Some("completado".to_string());

        store.append_run(&r1_pending).expect("append pending");
        store.append_run(&r2_pending).expect("append r2");
        store.append_run(&r1_running).expect("append running");
        store.append_run(&r1_done).expect("append done");

        let runs = store.list_runs().expect("list runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], r1_done);
        assert_eq!(runs[1], r2_pending);
        assert_eq!(store.get_run("run-1").expect("get run-1"), Some(r1_done));
    }

    #[test]
    fn tolerates_corrupt_line_in_middle() {
        let dir = tempdir().expect("create tempdir");
        let store = JsonlStore::new(dir.path()).expect("init store");

        let r1 = sample_record("run-1", RunStatus::Pending, 1000);
        let r2 = sample_record("run-2", RunStatus::Done, 2000);

        store.append_run(&r1).expect("append r1");
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(store.file_path())
                .expect("open runs.jsonl");
            writeln!(file, "{{corrupto").expect("write corrupt line");
            writeln!(file).expect("write empty line");
        }
        store.append_run(&r2).expect("append r2");

        let runs = store.list_runs().expect("list runs");
        assert_eq!(runs, vec![r1.clone(), r2.clone()]);
        assert_eq!(store.get_run("run-1").expect("get run-1"), Some(r1));
        assert_eq!(store.get_run("run-2").expect("get run-2"), Some(r2));
    }

    #[test]
    fn tolerates_non_utf8_and_truncated_lines() {
        let dir = tempdir().expect("create tempdir");
        let store = JsonlStore::new(dir.path()).expect("init store");
        let r1 = sample_record("run-1", RunStatus::Done, 1000);
        store.append_run(&r1).expect("append r1");
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(store.file_path())
                .expect("open runs.jsonl");
            file.write_all(b"\xff\xfe basura\n{\"id\":\"a medias")
                .expect("write garbage");
        }
        // La escritura siguiente cierra la línea truncada y no se pierde.
        let r2 = sample_record("run-2", RunStatus::Done, 2000);
        store.append_run(&r2).expect("append r2");
        let runs = store.list_runs().expect("list runs con basura");
        assert_eq!(runs, vec![r1, r2]);
    }

    #[test]
    fn concurrent_appends_from_many_threads_never_mix_lines() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().to_path_buf();
        let handles: Vec<_> = (0..16)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    // Cada hilo con su propio JsonlStore: como procesos distintos.
                    let store = JsonlStore::new(&path).expect("init store");
                    for i in 0..40 {
                        let mut rec = sample_record(&format!("run-{t}-{i}"), RunStatus::Done, 1);
                        rec.output = Some("x".repeat(20_000));
                        store.append_run(&rec).expect("append");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("hilo");
        }
        let content = std::fs::read_to_string(path.join("runs.jsonl")).expect("leer");
        let mut lines = 0;
        for line in content.lines() {
            serde_json::from_str::<RunRecord>(line).expect("línea íntegra");
            lines += 1;
        }
        assert_eq!(lines, 16 * 40);
    }
}
