use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderQuota {
    pub provider: String,
    pub display_name: String,
    pub status: String,
    pub source: String,
    pub windows: Vec<QuotaWindow>,
    pub used_percent: Option<f64>,
    pub balance: Option<f64>,
    pub currency: Option<String>,
    pub resets_at: Option<String>,
    pub updated_at: Option<String>,
    pub fetched_at_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManualEntry {
    pub percent_remaining: Option<f64>,
    pub balance: Option<f64>,
    pub currency: Option<String>,
    pub resets_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManualQuotaFile {
    #[serde(default)]
    pub providers: BTreeMap<String, ManualEntry>,
}

pub const PROVIDERS: [(&str, &str); 5] = [
    ("anthropic", "Anthropic"),
    ("copilot", "Copilot"),
    ("deepseek", "DeepSeek"),
    ("nous", "Nous"),
    ("gemini", "Gemini"),
];

pub trait UsageCommand: Send + Sync {
    fn usage_json(&self, provider: &str) -> Result<String, String>;
}

#[derive(Debug, Clone, Default)]
pub struct HermesCli;

impl UsageCommand for HermesCli {
    fn usage_json(&self, provider: &str) -> Result<String, String> {
        let mut child = Command::new("hermes")
            .args(["usage", "--provider", provider, "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    format!("hermes no disponible: {err}")
                } else {
                    format!("error al ejecutar hermes: {err}")
                }
            })?;

        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("timeout ejecutando hermes usage (15s)".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("error esperando a hermes: {err}"));
                }
            }
        };

        let mut stdout_buf = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_end(&mut stdout_buf);
        }

        let mut stderr_buf = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut stderr_buf);
        }

        if !status.success() {
            let stderr_text = String::from_utf8_lossy(&stderr_buf).trim().to_string();
            if stderr_text.is_empty() {
                return Err(format!("hermes finalizó con código {:?}", status.code()));
            }
            return Err(stderr_text);
        }

        Ok(String::from_utf8_lossy(&stdout_buf).into_owned())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HermesUsage {
    pub windows: Vec<QuotaWindow>,
    pub fetched_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawHermesUsage {
    #[serde(default)]
    fetched_at: Option<String>,
    #[serde(default)]
    windows: Vec<QuotaWindow>,
    #[serde(default)]
    unavailable_reason: Option<String>,
}

pub fn parse_hermes_usage(json: &str) -> Result<HermesUsage, String> {
    let raw: RawHermesUsage = serde_json::from_str(json)
        .map_err(|err| format!("JSON inválido de hermes usage: {err}"))?;

    if raw.windows.is_empty() {
        let reason = raw
            .unavailable_reason
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("sin ventanas de uso disponibles");
        return Err(reason.to_string());
    }

    Ok(HermesUsage {
        windows: raw.windows,
        fetched_at: raw.fetched_at,
    })
}

pub fn default_manual_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home)
            .join(".config")
            .join("forja")
            .join("quota.json"),
        _ => PathBuf::from("./quota.json"),
    }
}

pub fn load_manual(path: &Path) -> Result<ManualQuotaFile, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ManualQuotaFile::default());
        }
        Err(err) => return Err(format!("error leyendo {}: {err}", path.display())),
    };

    serde_json::from_str(&content)
        .map_err(|err| format!("JSON inválido en {}: {err}", path.display()))
}

pub fn save_manual(path: &Path, file: &ManualQuotaFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("error creando directorio {}: {err}", parent.display()))?;
        }
    }

    let serialized = serde_json::to_string_pretty(file)
        .map_err(|err| format!("error serializando cuotas manuales: {err}"))?;

    std::fs::write(path, serialized)
        .map_err(|err| format!("error escribiendo {}: {err}", path.display()))
}

pub fn validate_manual(file: &ManualQuotaFile) -> Result<(), String> {
    for (provider, entry) in &file.providers {
        if !PROVIDERS.iter().any(|(id, _)| *id == provider.as_str()) {
            return Err(format!("proveedor desconocido: {provider}"));
        }

        if let Some(percent) = entry.percent_remaining {
            if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
                return Err(format!(
                    "percent_remaining fuera de rango (0..=100) para {provider}: {percent}"
                ));
            }
        }

        if let Some(balance) = entry.balance {
            if !balance.is_finite() || balance < 0.0 {
                return Err(format!(
                    "balance inválido (>= 0) para {provider}: {balance}"
                ));
            }
            let valid_currency = entry
                .currency
                .as_deref()
                .map(str::trim)
                .is_some_and(|c| !c.is_empty());
            if !valid_currency {
                return Err(format!(
                    "currency obligatoria y no vacía cuando hay balance para {provider}"
                ));
            }
        }
    }

    Ok(())
}

pub struct QuotaService {
    cmd: Arc<dyn UsageCommand>,
    manual_path: Mutex<PathBuf>,
    cache: Mutex<HashMap<String, (Instant, ProviderQuota)>>,
    refresh_lock: Mutex<()>,
    ttl: Duration,
}

impl QuotaService {
    pub fn new(cmd: Arc<dyn UsageCommand>, manual_path: PathBuf, ttl: Duration) -> Self {
        Self {
            cmd,
            manual_path: Mutex::new(manual_path),
            cache: Mutex::new(HashMap::new()),
            refresh_lock: Mutex::new(()),
            ttl,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(
            Arc::new(HermesCli),
            default_manual_path(),
            Duration::from_secs(60),
        )
    }

    pub fn manual_path(&self) -> PathBuf {
        self.manual_path
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_manual_path(&self, path: PathBuf) {
        {
            let mut guard = self.manual_path.lock().unwrap_or_else(|e| e.into_inner());
            *guard = path;
        }
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn get_all(&self) -> Vec<ProviderQuota> {
        let _refresh_guard = self.refresh_lock.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();

        let mut slots: Vec<Option<ProviderQuota>> = vec![None; PROVIDERS.len()];
        let mut missing: Vec<(usize, &'static str, &'static str)> = Vec::new();

        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            for (idx, (id, display_name)) in PROVIDERS.iter().enumerate() {
                if let Some((ts, quota)) = cache.get(*id) {
                    if now.saturating_duration_since(*ts) < self.ttl {
                        slots[idx] = Some(quota.clone());
                        continue;
                    }
                }
                missing.push((idx, *id, *display_name));
            }
        }

        if missing.is_empty() {
            return slots.into_iter().flatten().collect();
        }

        struct ProbeResult {
            idx: usize,
            id: &'static str,
            display_name: &'static str,
            fetched_at_ms: u64,
            hermes_res: Result<HermesUsage, String>,
        }

        // Sondear proveedores no cacheados en paralelo para evitar ~5 s secuenciales en frío.
        let cmd = Arc::clone(&self.cmd);
        let probed: Vec<ProbeResult> = std::thread::scope(|scope| {
            let handles: Vec<_> = missing
                .iter()
                .map(|&(idx, id, display_name)| {
                    let cmd = Arc::clone(&cmd);
                    scope.spawn(move || {
                        let fetched_at_ms = now_epoch_ms();
                        let hermes_res =
                            cmd.usage_json(id).and_then(|raw| parse_hermes_usage(&raw));
                        ProbeResult {
                            idx,
                            id,
                            display_name,
                            fetched_at_ms,
                            hermes_res,
                        }
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| match h.join() {
                    Ok(item) => item,
                    Err(_) => ProbeResult {
                        idx: 0,
                        id: "",
                        display_name: "",
                        fetched_at_ms: now_epoch_ms(),
                        hermes_res: Err("error interno en hilo de sondeo".to_string()),
                    },
                })
                .collect()
        });

        let mut manual_loaded: Option<Result<ManualQuotaFile, String>> = None;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());

        for ProbeResult {
            idx,
            id,
            display_name,
            fetched_at_ms,
            hermes_res,
        } in probed
        {
            if id.is_empty() {
                continue;
            }
            let quota = match hermes_res {
                Ok(usage) => {
                    let (used_percent, resets_at) = highest_used_window(&usage.windows);
                    ProviderQuota {
                        provider: id.to_string(),
                        display_name: display_name.to_string(),
                        status: "ok".to_string(),
                        source: "hermes".to_string(),
                        windows: usage.windows,
                        used_percent,
                        balance: None,
                        currency: None,
                        resets_at,
                        updated_at: usage.fetched_at,
                        fetched_at_ms,
                        error: None,
                    }
                }
                Err(hermes_err) => {
                    if id == "anthropic" {
                        no_data_quota(id, display_name, fetched_at_ms, hermes_err)
                    } else {
                        let manual_res =
                            manual_loaded.get_or_insert_with(|| load_manual(&self.manual_path()));
                        match manual_res {
                            Ok(manual_file) => match manual_file.providers.get(id) {
                                Some(entry) => ProviderQuota {
                                    provider: id.to_string(),
                                    display_name: display_name.to_string(),
                                    status: "ok".to_string(),
                                    source: "manual".to_string(),
                                    windows: Vec::new(),
                                    used_percent: entry.percent_remaining.map(|rem| 100.0 - rem),
                                    balance: entry.balance,
                                    currency: entry.currency.clone(),
                                    resets_at: entry.resets_at.clone(),
                                    updated_at: entry.updated_at.clone(),
                                    fetched_at_ms,
                                    error: None,
                                },
                                None => no_data_quota(id, display_name, fetched_at_ms, hermes_err),
                            },
                            Err(manual_err) => no_data_quota(
                                id,
                                display_name,
                                fetched_at_ms,
                                format!("{hermes_err}; fallback manual falló: {manual_err}"),
                            ),
                        }
                    }
                }
            };

            cache.insert(id.to_string(), (Instant::now(), quota.clone()));
            if let Some(slot) = slots.get_mut(idx) {
                *slot = Some(quota);
            }
        }

        slots.into_iter().flatten().collect()
    }

    pub fn manual(&self) -> Result<ManualQuotaFile, String> {
        let path = self.manual_path();
        load_manual(&path)
    }

    pub fn put_manual(&self, file: ManualQuotaFile) -> Result<(), String> {
        validate_manual(&file)?;
        let path = self.manual_path();
        save_manual(&path, &file)?;
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
        Ok(())
    }
}

fn highest_used_window(windows: &[QuotaWindow]) -> (Option<f64>, Option<String>) {
    let mut best: Option<(f64, Option<String>)> = None;
    for w in windows {
        if let Some(used) = w.used_percent.filter(|v| v.is_finite()) {
            let is_higher = match &best {
                None => true,
                Some((current_max, _)) => used > *current_max,
            };
            if is_higher {
                best = Some((used, w.resets_at.clone()));
            }
        }
    }
    match best {
        Some((used, resets_at)) => (Some(used), resets_at),
        None => (None, None),
    }
}

fn no_data_quota(
    provider: &str,
    display_name: &str,
    fetched_at_ms: u64,
    error: String,
) -> ProviderQuota {
    ProviderQuota {
        provider: provider.to_string(),
        display_name: display_name.to_string(),
        status: "no_data".to_string(),
        source: "none".to_string(),
        windows: Vec::new(),
        used_percent: None,
        balance: None,
        currency: None,
        resets_at: None,
        updated_at: None,
        fetched_at_ms,
        error: Some(error),
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{
        parse_hermes_usage, save_manual, validate_manual, ManualEntry, ManualQuotaFile,
        QuotaService, UsageCommand, PROVIDERS,
    };

    const FIXTURE_ANTHROPIC: &str = include_str!("../tests/fixtures/hermes_usage_anthropic.json");
    const FIXTURE_BROKEN: &str = include_str!("../tests/fixtures/hermes_usage_broken.json");
    const FIXTURE_UNAVAILABLE: &str =
        include_str!("../tests/fixtures/hermes_usage_unavailable.json");

    type FakeHandler = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

    struct FakeUsageCommand {
        calls: AtomicUsize,
        handler: FakeHandler,
    }

    impl FakeUsageCommand {
        fn new<F>(handler: F) -> Self
        where
            F: Fn(&str) -> Result<String, String> + Send + Sync + 'static,
        {
            Self {
                calls: AtomicUsize::new(0),
                handler: Box::new(handler),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl UsageCommand for FakeUsageCommand {
        fn usage_json(&self, provider: &str) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.handler)(provider)
        }
    }

    #[test]
    fn quota_parse_hermes_fixture_real() {
        let usage = parse_hermes_usage(FIXTURE_ANTHROPIC).expect("debe parsear fixture real");
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "Current session");
        assert_eq!(usage.windows[0].used_percent, Some(4.0));
        assert_eq!(usage.windows[1].label, "Current week");
        assert_eq!(usage.windows[1].used_percent, Some(65.0));
        assert_eq!(
            usage.fetched_at.as_deref(),
            Some("2026-11-26T10:23:21.949515+00:00")
        );
    }

    #[test]
    fn quota_parse_hermes_broken_json() {
        let err = parse_hermes_usage(FIXTURE_BROKEN).expect_err("JSON roto debe fallar");
        assert!(err.contains("JSON inválido"), "error inesperado: {err}");
    }

    #[test]
    fn quota_parse_hermes_unavailable() {
        let err =
            parse_hermes_usage(FIXTURE_UNAVAILABLE).expect_err("windows vacío debe devolver error");
        assert!(
            err.contains("No account usage available for provider 'copilot'"),
            "error inesperado: {err}"
        );
    }

    #[test]
    fn quota_service_command_missing() {
        let dir = TempDir::new().expect("crear tempdir");
        let missing_manual = dir.path().join("no_existe.json");
        let fake = Arc::new(FakeUsageCommand::new(|_| {
            Err("hermes no disponible".to_string())
        }));

        let svc = QuotaService::new(fake, missing_manual, Duration::from_secs(60));
        let all = svc.get_all();
        assert_eq!(all.len(), PROVIDERS.len());

        for (quota, (expected_id, expected_name)) in all.iter().zip(PROVIDERS.iter()) {
            assert_eq!(quota.provider, *expected_id);
            assert_eq!(quota.display_name, *expected_name);
            assert_eq!(quota.status, "no_data");
            assert_eq!(quota.source, "none");
            assert!(quota.windows.is_empty());
            assert_eq!(quota.used_percent, None);
            assert_eq!(quota.balance, None);
            assert_eq!(quota.currency, None);
            assert!(quota
                .error
                .as_deref()
                .is_some_and(|e| e.contains("hermes no disponible")));
        }
    }

    #[test]
    fn quota_service_manual_fallback() {
        let dir = TempDir::new().expect("crear tempdir");
        let manual_path = dir.path().join("quota.json");

        let mut providers = BTreeMap::new();
        providers.insert(
            "copilot".to_string(),
            ManualEntry {
                percent_remaining: Some(5.0),
                balance: None,
                currency: None,
                resets_at: Some("2026-12-01T00:00:00Z".to_string()),
                updated_at: Some("2026-11-26T05:00:00Z".to_string()),
            },
        );
        providers.insert(
            "deepseek".to_string(),
            ManualEntry {
                percent_remaining: None,
                balance: Some(78.24),
                currency: Some("USD".to_string()),
                resets_at: None,
                updated_at: Some("2026-11-26T05:00:00Z".to_string()),
            },
        );
        providers.insert(
            "anthropic".to_string(),
            ManualEntry {
                percent_remaining: Some(50.0),
                ..ManualEntry::default()
            },
        );
        save_manual(&manual_path, &ManualQuotaFile { providers }).expect("guardar quota.json");

        let fake = Arc::new(FakeUsageCommand::new(|provider| {
            Err(format!("sin datos hermes para {provider}"))
        }));
        let svc = QuotaService::new(fake, manual_path, Duration::from_secs(60));
        let all = svc.get_all();

        let anthropic = all
            .iter()
            .find(|q| q.provider == "anthropic")
            .expect("anthropic presente");
        assert_eq!(anthropic.status, "no_data");
        assert_eq!(anthropic.source, "none");
        assert_eq!(anthropic.used_percent, None);

        let copilot = all
            .iter()
            .find(|q| q.provider == "copilot")
            .expect("copilot presente");
        assert_eq!(copilot.status, "ok");
        assert_eq!(copilot.source, "manual");
        assert_eq!(copilot.used_percent, Some(95.0));
        assert_eq!(copilot.balance, None);
        assert_eq!(copilot.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        assert_eq!(copilot.error, None);

        let deepseek = all
            .iter()
            .find(|q| q.provider == "deepseek")
            .expect("deepseek presente");
        assert_eq!(deepseek.status, "ok");
        assert_eq!(deepseek.source, "manual");
        assert_eq!(deepseek.used_percent, None);
        assert_eq!(deepseek.balance, Some(78.24));
        assert_eq!(deepseek.currency.as_deref(), Some("USD"));
        assert_eq!(deepseek.error, None);
    }

    #[test]
    fn quota_service_caches_results() {
        let dir = TempDir::new().expect("crear tempdir");
        let manual_path = dir.path().join("quota.json");
        let fake = Arc::new(FakeUsageCommand::new(|provider| {
            if provider == "anthropic" {
                Ok(FIXTURE_ANTHROPIC.to_string())
            } else {
                Err(format!("sin datos para {provider}"))
            }
        }));

        let svc = QuotaService::new(fake.clone(), manual_path, Duration::from_secs(60));
        let first = svc.get_all();
        assert_eq!(fake.call_count(), PROVIDERS.len());

        let anthropic = &first[0];
        assert_eq!(anthropic.provider, "anthropic");
        assert_eq!(anthropic.status, "ok");
        assert_eq!(anthropic.source, "hermes");
        assert_eq!(anthropic.used_percent, Some(65.0));
        assert_eq!(
            anthropic.resets_at.as_deref(),
            Some("2026-11-26T22:59:59.872334+00:00")
        );

        let second = svc.get_all();
        assert_eq!(fake.call_count(), PROVIDERS.len());
        assert_eq!(first, second);
    }

    #[test]
    fn quota_validate_rejects_invalid() {
        let mut percent_invalid = ManualQuotaFile::default();
        percent_invalid.providers.insert(
            "copilot".to_string(),
            ManualEntry {
                percent_remaining: Some(150.0),
                ..ManualEntry::default()
            },
        );
        assert!(validate_manual(&percent_invalid).is_err());

        let mut balance_no_currency = ManualQuotaFile::default();
        balance_no_currency.providers.insert(
            "deepseek".to_string(),
            ManualEntry {
                balance: Some(12.5),
                currency: None,
                ..ManualEntry::default()
            },
        );
        assert!(validate_manual(&balance_no_currency).is_err());

        let mut unknown_provider = ManualQuotaFile::default();
        unknown_provider.providers.insert(
            "openai".to_string(),
            ManualEntry {
                percent_remaining: Some(20.0),
                ..ManualEntry::default()
            },
        );
        assert!(validate_manual(&unknown_provider).is_err());
    }

    #[test]
    fn quota_concurrent_get_all_deduplicates_calls() {
        let dir = TempDir::new().expect("tempdir");
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_ref = Arc::clone(&calls);

        struct SlowFake(Arc<AtomicUsize>);
        impl UsageCommand for SlowFake {
            fn usage_json(&self, _provider: &str) -> Result<String, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                Err("sin dato".to_string())
            }
        }

        let svc = Arc::new(QuotaService::new(
            Arc::new(SlowFake(calls_ref)),
            dir.path().join("quota.json"),
            Duration::from_secs(60),
        ));

        let s1 = Arc::clone(&svc);
        let s2 = Arc::clone(&svc);
        let h1 = std::thread::spawn(move || s1.get_all());
        let h2 = std::thread::spawn(move || s2.get_all());
        assert_eq!(h1.join().expect("h1").len(), PROVIDERS.len());
        assert_eq!(h2.join().expect("h2").len(), PROVIDERS.len());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            PROVIDERS.len(),
            "dos llamadas simultáneas no deben duplicar subprocesos hermes"
        );
    }
}
