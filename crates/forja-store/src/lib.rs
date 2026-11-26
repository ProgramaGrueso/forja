use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};

use forja_core::RunRecord;

#[derive(Debug, Clone)]
pub struct JsonlStore {
    dir: PathBuf,
    file_path: PathBuf,
}

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

    pub fn append_run(&self, record: &RunRecord) -> forja_core::Result<()> {
        let line = serde_json::to_string(record)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    pub fn list_runs(&self) -> forja_core::Result<Vec<RunRecord>> {
        let file = match File::open(&self.file_path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };

        let reader = BufReader::new(file);
        let mut runs: Vec<RunRecord> = Vec::new();
        let mut indices: HashMap<String, usize> = HashMap::new();

        for line_result in reader.lines() {
            let line = line_result?;
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<RunRecord>(&line) else {
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
}
