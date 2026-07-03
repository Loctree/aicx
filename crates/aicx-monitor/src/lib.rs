//! Live system monitor for aicx pipelines. Vibecrafted. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
//!
//! Foundation crate exposing a [`tokio::sync::watch::Receiver<MonitorSnapshot>`]
//! sampler. Future TUI / dashboard consumers attach to the receiver and render
//! whatever they want — this crate is pure data plane.
//!
//! Ported from `rust-memex/src/tui/monitor.rs` with two operational deltas:
//! - the parent-process metrics are named `aicx_cpu` / `aicx_rss` (was
//!   `rust_memex_cpu` / `rust_memex_rss`).
//! - [`EMBEDDER_PROCESS_NAMES`] adds `vllm` and `qwen3-embedding` because
//!   today's aicx production loop drives `qwen3-embedding:8b` through
//!   Ollama / vLLM, and we want both code paths to be visible to the
//!   operator without a code change.

use std::ffi::{OsStr, OsString};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Apple-Silicon GPU IOKit classes we probe via `ioreg` on macOS.
pub const GPU_CLASSES: &[&str] = &["AGXAcceleratorG15X", "IOAccelerator"];

/// Process names treated as embedder workers for aggregate CPU/RAM rollup.
///
/// `vllm` and `qwen3-embedding` are aicx-specific additions on top of the
/// rust-memex baseline — today's `aicx index` runs are observed driving
/// `qwen3-embedding:8b` through Ollama / vLLM and the operator needs both
/// surfaces visible without recompiling.
pub const EMBEDDER_PROCESS_NAMES: &[&str] = &[
    "ollama",
    "llama-server",
    "mlx_server",
    "mlx-server",
    "vllm",
    "qwen3-embedding",
];

/// GPU probe status for the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuStatus {
    Available { class_name: String },
    Unavailable { reason: String },
}

impl Default for GpuStatus {
    fn default() -> Self {
        Self::Unavailable {
            reason: "not sampled yet".to_string(),
        }
    }
}

/// Latest system monitor snapshot for the dashboard.
#[derive(Debug, Clone)]
pub struct MonitorSnapshot {
    pub system_cpu_percent: f32,
    pub system_ram_used: u64,
    pub system_ram_total: u64,
    /// CPU% attributable to the aicx parent process itself.
    pub aicx_cpu: f32,
    /// RSS bytes attributable to the aicx parent process itself.
    pub aicx_rss: u64,
    pub embedder_cpu_aggregate: f32,
    pub embedder_rss_aggregate: u64,
    /// Number of embedder processes detected. Useful for the "is Ollama
    /// running at all?" liveness check during a 75-minute `aicx index` run.
    pub embedder_process_count: usize,
    pub gpu_util_percent: Option<f32>,
    pub gpu_memory_used: Option<u64>,
    pub gpu_memory_total: Option<u64>,
    pub gpu_status: GpuStatus,
    pub sampled_at: Instant,
}

impl Default for MonitorSnapshot {
    fn default() -> Self {
        Self {
            system_cpu_percent: 0.0,
            system_ram_used: 0,
            system_ram_total: 0,
            aicx_cpu: 0.0,
            aicx_rss: 0,
            embedder_cpu_aggregate: 0.0,
            embedder_rss_aggregate: 0,
            embedder_process_count: 0,
            gpu_util_percent: None,
            gpu_memory_used: None,
            gpu_memory_total: None,
            gpu_status: GpuStatus::default(),
            sampled_at: Instant::now(),
        }
    }
}

impl MonitorSnapshot {
    /// Human-readable byte formatter (B / KB / MB / GB).
    pub fn format_bytes(bytes: u64) -> String {
        const KB: f64 = 1024.0;
        const MB: f64 = KB * 1024.0;
        const GB: f64 = MB * 1024.0;

        match bytes {
            0..=1023 => format!("{bytes} B"),
            1_024..=1_048_575 => format!("{:.0} KB", bytes as f64 / KB),
            1_048_576..=1_073_741_823 => format!("{:.0} MB", bytes as f64 / MB),
            _ => format!("{:.1} GB", bytes as f64 / GB),
        }
    }
}

/// Spawn the system monitor sampler with a latest-value watch channel.
///
/// The returned [`tokio::task::JoinHandle`] runs until every receiver is
/// dropped, at which point the loop exits cleanly on the next tick.
pub fn spawn_monitor(interval: Duration) -> (watch::Receiver<MonitorSnapshot>, JoinHandle<()>) {
    let (sender, receiver) = watch::channel(MonitorSnapshot::default());
    let handle = tokio::spawn(async move {
        let my_pid = Pid::from_u32(std::process::id());
        let mut system = System::new_all();
        system.refresh_all();
        // Send a liveness snapshot immediately so dashboards/tests do not
        // stare at the default zero state while CPU deltas warm up. The next
        // tick gets the more meaningful per-process CPU reading.
        if sender.send(build_snapshot(&system, my_pid)).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;

        loop {
            system.refresh_cpu_usage();
            system.refresh_memory();
            system.refresh_processes(ProcessesToUpdate::All, true);

            let snapshot = build_snapshot(&system, my_pid);
            if sender.send(snapshot).is_err() {
                break;
            }
            tokio::time::sleep(interval).await;
        }
    });

    (receiver, handle)
}

fn build_snapshot(system: &System, my_pid: Pid) -> MonitorSnapshot {
    let mut snapshot = MonitorSnapshot {
        system_cpu_percent: system.global_cpu_usage(),
        system_ram_used: system.used_memory(),
        system_ram_total: system.total_memory(),
        sampled_at: Instant::now(),
        ..MonitorSnapshot::default()
    };

    if let Some(process) = system.process(my_pid) {
        snapshot.aicx_cpu = process.cpu_usage();
        snapshot.aicx_rss = process.memory();
    }

    for process in system.processes().values() {
        if is_embedder_process(process.name(), process.cmd()) {
            snapshot.embedder_cpu_aggregate += process.cpu_usage();
            snapshot.embedder_rss_aggregate += process.memory();
            snapshot.embedder_process_count += 1;
        }
    }

    match probe_gpu() {
        Ok(metrics) => {
            snapshot.gpu_util_percent = Some(metrics.device_util as f32);
            snapshot.gpu_memory_used = metrics.memory_used;
            snapshot.gpu_memory_total = metrics.memory_total;
            snapshot.gpu_status = GpuStatus::Available {
                class_name: metrics.class_name,
            };
        }
        Err(status) => {
            snapshot.gpu_status = status;
        }
    }

    snapshot
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GpuMetrics {
    class_name: String,
    device_util: u64,
    memory_used: Option<u64>,
    memory_total: Option<u64>,
}

#[cfg(target_os = "macos")]
fn probe_gpu() -> Result<GpuMetrics, GpuStatus> {
    let mut reasons = Vec::new();

    for class_name in GPU_CLASSES {
        match std::process::Command::new("ioreg")
            .args(["-l", "-w", "0", "-r", "-c", class_name, "-d", "1"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(metrics) = parse_ioreg_output(&stdout, class_name) {
                    return Ok(metrics);
                }
                reasons.push(format!("{class_name}: telemetry keys not found"));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if stderr.is_empty() {
                    reasons.push(format!("{class_name}: ioreg exited with {}", output.status));
                } else {
                    reasons.push(format!("{class_name}: {stderr}"));
                }
            }
            Err(error) => {
                reasons.push(format!("{class_name}: {error}"));
            }
        }
    }

    Err(GpuStatus::Unavailable {
        reason: reasons.join(" | "),
    })
}

#[cfg(not(target_os = "macos"))]
fn probe_gpu() -> Result<GpuMetrics, GpuStatus> {
    Err(GpuStatus::Unavailable {
        reason: "GPU detection not implemented for this OS".to_string(),
    })
}

/// Detect whether an Apple-Silicon GPU is currently accessible via `ioreg`.
///
/// Returns [`GpuStatus::Available`] with the matched IOKit class name on
/// success, or [`GpuStatus::Unavailable`] with a diagnostic reason on
/// failure / non-macOS hosts.
pub fn is_apple_gpu_available() -> GpuStatus {
    match probe_gpu() {
        Ok(metrics) => GpuStatus::Available {
            class_name: metrics.class_name,
        },
        Err(status) => status,
    }
}

#[cfg(target_os = "macos")]
fn parse_ioreg_output(output: &str, class_name: &str) -> Option<GpuMetrics> {
    let device_util = extract_ioreg_value(output, "Device Utilization %")
        .or_else(|| extract_ioreg_value(output, "Renderer Utilization %"))?;

    Some(GpuMetrics {
        class_name: class_name.to_string(),
        device_util,
        memory_used: extract_ioreg_value(output, "In use system memory"),
        memory_total: extract_ioreg_value(output, "Alloc system memory"),
    })
}

#[cfg(target_os = "macos")]
fn extract_ioreg_value(output: &str, key: &str) -> Option<u64> {
    let quoted_key = format!("\"{key}\"");
    let key_index = output.find(&quoted_key)?;
    let value_region = &output[key_index + quoted_key.len()..];
    let equals_index = value_region.find('=')?;
    let remainder = value_region[equals_index + 1..].trim_start();
    let digits: String = remainder
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();

    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn is_embedder_process(name: &OsStr, cmdline: &[OsString]) -> bool {
    let name = name.to_string_lossy().to_lowercase();
    if EMBEDDER_PROCESS_NAMES
        .iter()
        .any(|candidate| name.contains(candidate))
    {
        return true;
    }

    if name.contains("python") {
        let cmdline = cmdline
            .iter()
            .map(|segment| segment.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        return cmdline.contains("mlx") || cmdline.contains("embed");
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_renders_human_readable_units() {
        assert_eq!(MonitorSnapshot::format_bytes(0), "0 B");
        assert_eq!(MonitorSnapshot::format_bytes(1023), "1023 B");
        assert_eq!(MonitorSnapshot::format_bytes(1024), "1 KB");
        assert_eq!(MonitorSnapshot::format_bytes(1_048_576), "1 MB");
        assert_eq!(MonitorSnapshot::format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn default_snapshot_is_zeroed_with_unavailable_gpu() {
        let snap = MonitorSnapshot::default();
        assert_eq!(snap.system_cpu_percent, 0.0);
        assert_eq!(snap.system_ram_used, 0);
        assert_eq!(snap.system_ram_total, 0);
        assert_eq!(snap.aicx_cpu, 0.0);
        assert_eq!(snap.aicx_rss, 0);
        assert_eq!(snap.embedder_cpu_aggregate, 0.0);
        assert_eq!(snap.embedder_rss_aggregate, 0);
        assert_eq!(snap.embedder_process_count, 0);
        assert!(snap.gpu_util_percent.is_none());
        assert!(snap.gpu_memory_used.is_none());
        assert!(snap.gpu_memory_total.is_none());
        assert!(matches!(snap.gpu_status, GpuStatus::Unavailable { .. }));
    }

    #[test]
    fn embedder_process_names_cover_aicx_runtime() {
        assert!(EMBEDDER_PROCESS_NAMES.contains(&"ollama"));
        assert!(EMBEDDER_PROCESS_NAMES.contains(&"vllm"));
        assert!(EMBEDDER_PROCESS_NAMES.contains(&"qwen3-embedding"));
    }

    #[test]
    fn embedder_process_detection_matches_expected_names() {
        assert!(is_embedder_process(OsStr::new("ollama"), &[]));
        assert!(is_embedder_process(OsStr::new("llama-server"), &[]));
        assert!(is_embedder_process(OsStr::new("mlx_server"), &[]));
        assert!(is_embedder_process(OsStr::new("vllm"), &[]));
        assert!(is_embedder_process(OsStr::new("qwen3-embedding"), &[]));
        assert!(is_embedder_process(
            OsStr::new("python3"),
            &[
                OsString::from("python3"),
                OsString::from("-m"),
                OsString::from("mlx.embed"),
            ]
        ));
        assert!(!is_embedder_process(OsStr::new("nginx"), &[]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_monitor_yields_live_snapshot() {
        let (mut rx, handle) = spawn_monitor(Duration::from_millis(100));

        // Wait for a non-default snapshot. system_ram_total > 0 is a robust
        // liveness signal — every host running this test has RAM.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut got_live = false;
        loop {
            if rx.borrow().system_ram_total > 0 {
                got_live = true;
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            // Wait for the next sample, but never longer than the deadline.
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let _ = tokio::time::timeout(remaining, rx.changed()).await;
        }

        // Drop the receiver so the spawned sampler exits cleanly.
        drop(rx);
        handle.abort();
        let _ = handle.await;

        assert!(
            got_live,
            "spawn_monitor did not produce a live snapshot within 2s"
        );
    }
}
