use std::collections::VecDeque;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuSample {
    pub name: String,
    pub memory_used_mib: u64,
    pub memory_total_mib: u64,
    pub utilization_percent: f64,
    pub temperature_c: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelemetrySample {
    pub ts_ms: u64,
    pub cpu_percent: Option<f64>,
    pub ram_used_kib: Option<u64>,
    pub ram_total_kib: Option<u64>,
    pub gpu: Option<GpuSample>,
    pub gpu_error: Option<String>,
    pub system_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySnapshot {
    pub latest: Option<TelemetrySample>,
    pub history: Vec<TelemetrySample>,
    pub interval_ms: u64,
    pub capacity: usize,
}

pub trait GpuSource: Send + Sync {
    fn read(&self) -> Result<GpuSample, String>;
}

pub trait SystemSource: Send + Sync {
    fn proc_stat(&self) -> Result<String, String>;
    fn meminfo(&self) -> Result<String, String>;
}

pub struct NvidiaSmiGpu;

impl GpuSource for NvidiaSmiGpu {
    fn read(&self) -> Result<GpuSample, String> {
        let output = Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,memory.used,memory.total,utilization.gpu,temperature.gpu",
                "--format=csv,noheader,nounits",
            ])
            .output()
            .map_err(|e| format!("GPU no disponible: no se pudo ejecutar nvidia-smi: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("estado {}", output.status)
            } else {
                format!("estado {}: {stderr}", output.status)
            };
            return Err(format!("GPU no disponible: nvidia-smi falló ({detail})"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_line = stdout
            .lines()
            .find(|line| !line.trim().is_empty())
            .ok_or_else(|| "GPU no disponible: nvidia-smi no devolvió datos".to_string())?;

        parse_nvidia_smi_line(first_line).map_err(|e| format!("GPU no disponible: {e}"))
    }
}

pub struct ProcSystem;

impl SystemSource for ProcSystem {
    fn proc_stat(&self) -> Result<String, String> {
        std::fs::read_to_string("/proc/stat").map_err(|e| format!("error leyendo /proc/stat: {e}"))
    }

    fn meminfo(&self) -> Result<String, String> {
        std::fs::read_to_string("/proc/meminfo")
            .map_err(|e| format!("error leyendo /proc/meminfo: {e}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

pub fn parse_nvidia_smi_line(line: &str) -> Result<GpuSample, String> {
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() != 5 {
        return Err(format!(
            "línea de nvidia-smi inválida: se esperaban 5 campos, se obtuvieron {}",
            parts.len()
        ));
    }

    for part in &parts {
        if part.is_empty() || part.eq_ignore_ascii_case("[N/A]") || part.eq_ignore_ascii_case("N/A")
        {
            return Err(format!("valor no disponible en nvidia-smi: '{part}'"));
        }
    }

    let name = parts[0].to_string();
    let memory_used_mib = parts[1]
        .parse::<u64>()
        .map_err(|e| format!("memory.used inválido '{}': {e}", parts[1]))?;
    let memory_total_mib = parts[2]
        .parse::<u64>()
        .map_err(|e| format!("memory.total inválido '{}': {e}", parts[2]))?;
    let utilization_percent = parse_finite_f64(parts[3], "utilization.gpu")?;
    let temperature_c = parse_finite_f64(parts[4], "temperature.gpu")?;

    Ok(GpuSample {
        name,
        memory_used_mib,
        memory_total_mib,
        utilization_percent,
        temperature_c,
    })
}

fn parse_finite_f64(raw: &str, field: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|e| format!("{field} inválido '{raw}': {e}"))?;
    if !value.is_finite() {
        return Err(format!("{field} no es un número finito: '{raw}'"));
    }
    Ok(value)
}

pub fn parse_cpu_times(proc_stat: &str) -> Result<CpuTimes, String> {
    let cpu_line = proc_stat
        .lines()
        .find(|line| line.starts_with("cpu "))
        .ok_or_else(|| "línea agregada 'cpu ' no encontrada en /proc/stat".to_string())?;

    let mut values = Vec::new();
    for token in cpu_line.split_whitespace().skip(1) {
        let parsed = token
            .parse::<u64>()
            .map_err(|e| format!("valor de CPU inválido '{token}' en /proc/stat: {e}"))?;
        values.push(parsed);
    }

    if values.len() < 4 {
        return Err(format!(
            "línea 'cpu ' incompleta en /proc/stat: se esperaban al menos 4 campos, hay {}",
            values.len()
        ));
    }

    let total = values.iter().copied().fold(0u64, u64::saturating_add);
    let idle = values[3].saturating_add(values.get(4).copied().unwrap_or(0));

    Ok(CpuTimes { total, idle })
}

pub fn cpu_percent_between(prev: CpuTimes, cur: CpuTimes) -> Option<f64> {
    let total_delta = cur.total.saturating_sub(prev.total);
    if total_delta == 0 {
        return None;
    }

    let idle_delta = cur.idle.saturating_sub(prev.idle);
    let active_delta = total_delta.saturating_sub(idle_delta);
    let pct = (active_delta as f64 / total_delta as f64) * 100.0;
    Some(pct.clamp(0.0, 100.0))
}

pub fn parse_meminfo(meminfo: &str) -> Result<(u64, u64), String> {
    let mut mem_total: Option<u64> = None;
    let mut mem_available: Option<u64> = None;

    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            mem_total = Some(parse_meminfo_kib(rest, "MemTotal")?);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            mem_available = Some(parse_meminfo_kib(rest, "MemAvailable")?);
        }
    }

    let total = mem_total.ok_or_else(|| "MemTotal no encontrado en /proc/meminfo".to_string())?;
    let available =
        mem_available.ok_or_else(|| "MemAvailable no encontrado en /proc/meminfo".to_string())?;
    let used = total.saturating_sub(available);

    Ok((total, used))
}

fn parse_meminfo_kib(rest: &str, field: &str) -> Result<u64, String> {
    let token = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("{field} sin valor en /proc/meminfo"))?;
    token
        .parse::<u64>()
        .map_err(|e| format!("{field} inválido '{token}' en /proc/meminfo: {e}"))
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

struct TelemetryState {
    prev_cpu: Option<CpuTimes>,
    history: VecDeque<TelemetrySample>,
}

struct TelemetryInner {
    gpu: Arc<dyn GpuSource>,
    system: Arc<dyn SystemSource>,
    interval_ms: u64,
    capacity: usize,
    state: Mutex<TelemetryState>,
}

#[derive(Clone)]
pub struct Telemetry {
    inner: Arc<TelemetryInner>,
}

impl Telemetry {
    pub fn new(
        gpu: Arc<dyn GpuSource>,
        system: Arc<dyn SystemSource>,
        capacity: usize,
        interval_ms: u64,
    ) -> Self {
        Self {
            inner: Arc::new(TelemetryInner {
                gpu,
                system,
                interval_ms,
                capacity,
                state: Mutex::new(TelemetryState {
                    prev_cpu: None,
                    history: VecDeque::with_capacity(capacity),
                }),
            }),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(Arc::new(NvidiaSmiGpu), Arc::new(ProcSystem), 150, 2000)
    }

    pub fn sample_once(&self) -> TelemetrySample {
        let ts_ms = now_epoch_ms();

        let mut system_errors = Vec::new();

        let cpu_reading = match self.inner.system.proc_stat() {
            Ok(raw) => match parse_cpu_times(&raw) {
                Ok(times) => Some(times),
                Err(err) => {
                    system_errors.push(err);
                    None
                }
            },
            Err(err) => {
                system_errors.push(err);
                None
            }
        };

        let (ram_total_kib, ram_used_kib) = match self.inner.system.meminfo() {
            Ok(raw) => match parse_meminfo(&raw) {
                Ok((total, used)) => (Some(total), Some(used)),
                Err(err) => {
                    system_errors.push(err);
                    (None, None)
                }
            },
            Err(err) => {
                system_errors.push(err);
                (None, None)
            }
        };

        let system_error = if system_errors.is_empty() {
            None
        } else {
            Some(system_errors.join("; "))
        };

        let (gpu, gpu_error) = match self.inner.gpu.read() {
            Ok(sample) => (Some(sample), None),
            Err(err) => (None, Some(err)),
        };

        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());

        let cpu_percent = match cpu_reading {
            Some(cur) => {
                let pct = state
                    .prev_cpu
                    .and_then(|prev| cpu_percent_between(prev, cur));
                state.prev_cpu = Some(cur);
                pct
            }
            None => None,
        };

        let sample = TelemetrySample {
            ts_ms,
            cpu_percent,
            ram_used_kib,
            ram_total_kib,
            gpu,
            gpu_error,
            system_error,
        };

        if self.inner.capacity > 0 {
            while state.history.len() >= self.inner.capacity {
                state.history.pop_front();
            }
            state.history.push_back(sample.clone());
        }

        sample
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());

        TelemetrySnapshot {
            latest: state.history.back().cloned(),
            history: state.history.iter().cloned().collect(),
            interval_ms: self.inner.interval_ms,
            capacity: self.inner.capacity,
        }
    }

    pub fn spawn_sampler(&self) -> tokio::task::JoinHandle<()> {
        let telemetry = self.clone();
        let interval = Duration::from_millis(self.inner.interval_ms);

        tokio::spawn(async move {
            loop {
                let sampler = telemetry.clone();
                let _ = tokio::task::spawn_blocking(move || sampler.sample_once()).await;
                tokio::time::sleep(interval).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{
        cpu_percent_between, parse_cpu_times, parse_meminfo, parse_nvidia_smi_line, CpuTimes,
        GpuSample, GpuSource, SystemSource, Telemetry,
    };

    struct FakeGpu {
        result: Result<GpuSample, String>,
    }

    impl GpuSource for FakeGpu {
        fn read(&self) -> Result<GpuSample, String> {
            self.result.clone()
        }
    }

    struct SequenceSystem {
        stats: Vec<String>,
        meminfo: String,
        idx: AtomicUsize,
    }

    impl SystemSource for SequenceSystem {
        fn proc_stat(&self) -> Result<String, String> {
            let i = self.idx.fetch_add(1, Ordering::SeqCst);
            let clamped = i.min(self.stats.len().saturating_sub(1));
            Ok(self.stats[clamped].clone())
        }

        fn meminfo(&self) -> Result<String, String> {
            Ok(self.meminfo.clone())
        }
    }

    fn sample_meminfo_str() -> String {
        "\
MemTotal:       16384000 kB
MemFree:         2000000 kB
MemAvailable:    6384000 kB
Buffers:          500000 kB
"
        .to_string()
    }

    #[test]
    fn telemetry_parse_nvidia_smi_valid() {
        let line = "NVIDIA GeForce RTX 4070 Ti SUPER, 2021, 16376, 16, 47.84";
        let sample = parse_nvidia_smi_line(line).expect("parseo válido de nvidia-smi");
        assert_eq!(sample.name, "NVIDIA GeForce RTX 4070 Ti SUPER");
        assert_eq!(sample.memory_used_mib, 2021);
        assert_eq!(sample.memory_total_mib, 16376);
        assert!((sample.utilization_percent - 16.0).abs() < f64::EPSILON);
        assert!((sample.temperature_c - 47.84).abs() < f64::EPSILON);
    }

    #[test]
    fn telemetry_parse_nvidia_smi_invalid() {
        assert!(parse_nvidia_smi_line("NVIDIA GPU, [N/A], 16376, 16, 47").is_err());
        assert!(parse_nvidia_smi_line("NVIDIA GPU, 2021, 16376, 16").is_err());
        assert!(parse_nvidia_smi_line("texto completamente basura").is_err());
        assert!(parse_nvidia_smi_line("NVIDIA GPU, 2021, 16376, NaN, 47").is_err());
        assert!(parse_nvidia_smi_line("[N/A], 2021, 16376, 16, 47").is_err());
    }

    #[test]
    fn telemetry_parse_cpu_and_percent() {
        let stat1 = "\
cpu  100 0 100 700 100 0 0 0 0 0
cpu0 50 0 50 350 50 0 0 0 0 0
";
        let stat2 = "\
cpu  150 0 150 750 150 0 0 0 0 0
cpu0 75 0 75 375 75 0 0 0 0 0
";

        let t1 = parse_cpu_times(stat1).expect("parseo stat1");
        let t2 = parse_cpu_times(stat2).expect("parseo stat2");

        assert_eq!(
            t1,
            CpuTimes {
                total: 1000,
                idle: 800
            }
        );
        assert_eq!(
            t2,
            CpuTimes {
                total: 1200,
                idle: 900
            }
        );

        // total delta = 200, idle delta = 100 => active = 100 => 50.0%
        let pct = cpu_percent_between(t1, t2).expect("porcentaje de CPU");
        assert!((pct - 50.0).abs() < 1e-9);

        // Delta cero => None
        assert_eq!(cpu_percent_between(t2, t2), None);
    }

    #[test]
    fn telemetry_parse_meminfo() {
        let raw = sample_meminfo_str();
        let (total, used) = parse_meminfo(&raw).expect("parseo de meminfo");
        assert_eq!(total, 16_384_000);
        assert_eq!(used, 10_000_000);

        let missing_available = "MemTotal:       16384000 kB\nMemFree:         2000000 kB\n";
        assert!(parse_meminfo(missing_available).is_err());
    }

    #[test]
    fn telemetry_sample_with_fake_sources() {
        let gpu = Arc::new(FakeGpu {
            result: Ok(GpuSample {
                name: "Fake RTX".to_string(),
                memory_used_mib: 1024,
                memory_total_mib: 8192,
                utilization_percent: 42.0,
                temperature_c: 55.5,
            }),
        });
        let system = Arc::new(SequenceSystem {
            stats: vec![
                "cpu  100 0 0 900 0 0 0 0\n".to_string(),
                "cpu  125 0 25 950 0 0 0 0\n".to_string(),
            ],
            meminfo: sample_meminfo_str(),
            idx: AtomicUsize::new(0),
        });

        let telemetry = Telemetry::new(gpu, system, 10, 2000);

        let first = telemetry.sample_once();
        assert_eq!(first.cpu_percent, None);
        assert_eq!(first.ram_total_kib, Some(16_384_000));
        assert_eq!(first.ram_used_kib, Some(10_000_000));
        assert_eq!(first.gpu_error, None);
        assert_eq!(first.system_error, None);
        assert_eq!(
            first.gpu.as_ref().map(|g| g.name.as_str()),
            Some("Fake RTX")
        );

        let second = telemetry.sample_once();
        // total delta = 100, idle delta = 50 => 50.0%
        let cpu = second.cpu_percent.expect("segunda muestra tiene CPU");
        assert!((cpu - 50.0).abs() < 1e-9);

        let snap = telemetry.snapshot();
        assert_eq!(snap.history.len(), 2);
        assert_eq!(snap.latest, Some(second));
        assert_eq!(snap.capacity, 10);
        assert_eq!(snap.interval_ms, 2000);
    }

    #[test]
    fn telemetry_gpu_unavailable_does_not_break() {
        let gpu = Arc::new(FakeGpu {
            result: Err("GPU no disponible: nvidia-smi no encontrado".to_string()),
        });
        let system = Arc::new(SequenceSystem {
            stats: vec!["cpu  100 0 0 900 0 0 0 0\n".to_string()],
            meminfo: sample_meminfo_str(),
            idx: AtomicUsize::new(0),
        });

        let telemetry = Telemetry::new(gpu, system, 10, 2000);
        let sample = telemetry.sample_once();

        assert_eq!(sample.gpu, None);
        assert!(sample
            .gpu_error
            .as_deref()
            .is_some_and(|e| e.contains("GPU no disponible")));
        assert_eq!(sample.ram_total_kib, Some(16_384_000));
        assert_eq!(sample.ram_used_kib, Some(10_000_000));
        assert_eq!(sample.system_error, None);
    }

    #[test]
    fn telemetry_buffer_respects_capacity() {
        let gpu = Arc::new(FakeGpu {
            result: Ok(GpuSample {
                name: "Fake GPU".to_string(),
                memory_used_mib: 500,
                memory_total_mib: 4000,
                utilization_percent: 10.0,
                temperature_c: 40.0,
            }),
        });
        let system = Arc::new(SequenceSystem {
            stats: vec![
                "cpu  100 0 0 900 0 0 0 0\n".to_string(),
                "cpu  110 0 0 990 0 0 0 0\n".to_string(), // 10%
                "cpu  130 0 0 1070 0 0 0 0\n".to_string(), // 20%
                "cpu  160 0 0 1140 0 0 0 0\n".to_string(), // 30%
                "cpu  200 0 0 1200 0 0 0 0\n".to_string(), // 40%
            ],
            meminfo: sample_meminfo_str(),
            idx: AtomicUsize::new(0),
        });

        let telemetry = Telemetry::new(gpu, system, 3, 1000);
        let mut samples = Vec::new();
        for _ in 0..5 {
            samples.push(telemetry.sample_once());
        }

        let snap = telemetry.snapshot();
        assert_eq!(snap.capacity, 3);
        assert_eq!(snap.history.len(), 3);
        assert_eq!(snap.history[0], samples[2]);
        assert_eq!(snap.history[1], samples[3]);
        assert_eq!(snap.history[2], samples[4]);
        assert_eq!(snap.latest, Some(samples[4].clone()));
    }
}
