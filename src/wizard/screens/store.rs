use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
enum StoreEvent {
    Line(String),
    Done(StoreOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreProgress {
    pub phase: String,
    pub current: u64,
    pub total: Option<u64>,
    pub status: String,
}

impl StoreProgress {
    pub fn ratio(&self) -> f64 {
        let Some(total) = self.total else {
            return 0.0;
        };
        if total == 0 {
            0.0
        } else {
            (self.current as f64 / total as f64).clamp(0.0, 1.0)
        }
    }
}

#[derive(Debug)]
pub struct StoreScreen {
    pub running: bool,
    pub log: Vec<String>,
    pub scroll: usize,
    pub status: String,
    pub hours: u64,
    pub progress: Option<StoreProgress>,
    rx: Option<mpsc::Receiver<StoreEvent>>,
    child: Option<Arc<Mutex<Option<Child>>>>,
    cancel_requested: Option<Arc<AtomicBool>>,
}

impl Default for StoreScreen {
    fn default() -> Self {
        Self {
            running: false,
            log: Vec::new(),
            scroll: 0,
            status: "store range: 48h".to_string(),
            hours: 48,
            progress: None,
            rx: None,
            child: None,
            cancel_requested: None,
        }
    }
}

impl StoreScreen {
    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn start(&mut self) {
        if self.running {
            self.status = "store run already in flight".to_string();
            return;
        }

        if self.hours == 0 {
            self.hours = 48;
        }

        let Ok(exe) = std::env::current_exe() else {
            self.status = "failed to resolve current aicx executable".to_string();
            return;
        };

        let (event_tx, event_rx) = mpsc::channel();
        self.rx = Some(event_rx);
        let cancel_requested = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel_requested.clone());
        self.running = true;
        self.log.clear();
        self.progress = None;
        self.log
            .push(format!("running: aicx store -H {} --emit none", self.hours));
        self.status = "store run started".to_string();

        let hours = self.hours.to_string();
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        self.child = Some(child_slot.clone());

        thread::spawn(move || {
            let mut child = match Command::new(exe)
                .arg("store")
                .arg("-H")
                .arg(hours)
                .arg("--emit")
                .arg("none")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    let _ = event_tx.send(StoreEvent::Line(format!("spawn failed: {error}")));
                    let _ = event_tx.send(StoreEvent::Done(StoreOutcome::Failed));
                    return;
                }
            };

            if let Some(stderr) = child.stderr.take() {
                let tx = event_tx.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let _ = tx.send(StoreEvent::Line(line));
                    }
                });
            }

            if let Some(stdout) = child.stdout.take() {
                let tx = event_tx.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        let _ = tx.send(StoreEvent::Line(line));
                    }
                });
            }

            {
                let mut guard = child_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *guard = Some(child);
            }

            loop {
                let outcome = {
                    let mut guard = child_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(child) = guard.as_mut() {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                *guard = None;
                                if cancel_requested.load(Ordering::SeqCst) {
                                    StoreOutcome::Cancelled
                                } else if status.success() {
                                    StoreOutcome::Completed
                                } else {
                                    StoreOutcome::Failed
                                }
                            }
                            Ok(None) => {
                                drop(guard);
                                thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                            Err(error) => {
                                let _ = event_tx
                                    .send(StoreEvent::Line(format!("wait failed: {error}")));
                                *guard = None;
                                StoreOutcome::Failed
                            }
                        }
                    } else {
                        StoreOutcome::Cancelled
                    }
                };
                let _ = event_tx.send(StoreEvent::Done(outcome));
                break;
            }
        });
    }

    pub fn poll(&mut self) {
        while let Some(event) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            match event {
                StoreEvent::Line(line) => {
                    self.update_progress_from_line(&line);
                    self.push_log(line);
                }
                StoreEvent::Done(outcome) => {
                    self.running = false;
                    self.child = None;
                    self.cancel_requested = None;
                    self.status = match outcome {
                        StoreOutcome::Completed => "store run completed".to_string(),
                        StoreOutcome::Failed => "store run failed".to_string(),
                        StoreOutcome::Cancelled => "store run cancelled".to_string(),
                    };
                }
            }
        }
    }

    pub fn cancel(&mut self) -> bool {
        if !self.running {
            return false;
        }

        if let Some(cancel_requested) = &self.cancel_requested {
            cancel_requested.store(true, Ordering::SeqCst);
        }

        let mut killed = false;
        if let Some(child_slot) = &self.child {
            let mut guard = child_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(child) = guard.as_mut() {
                killed = child.kill().is_ok();
            }
        }

        if killed {
            self.status = "store cancel requested; kill signal sent".to_string();
            self.push_log("cancel requested; kill signal sent to store subprocess".to_string());
        } else {
            self.status = "store cancel requested; subprocess already exiting".to_string();
            self.push_log("cancel requested; subprocess already exiting".to_string());
        }
        true
    }

    pub fn cycle_hours(&mut self) {
        const PRESETS: &[u64] = &[4, 24, 48, 168];
        if self.running {
            self.status = "store range is locked while a run is active".to_string();
            return;
        }
        let current = if self.hours == 0 { 48 } else { self.hours };
        let next = PRESETS
            .iter()
            .position(|preset| *preset == current)
            .map(|idx| PRESETS[(idx + 1) % PRESETS.len()])
            .unwrap_or(48);
        self.hours = next;
        self.status = format!("store range set to {next}h");
    }

    pub fn move_log(&mut self, delta: isize) {
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        } else {
            self.scroll = self
                .scroll
                .saturating_add(delta as usize)
                .min(self.log.len().saturating_sub(1));
        }
    }

    fn push_log(&mut self, line: String) {
        self.log.push(line);
        self.scroll = self.scroll.min(self.log.len().saturating_sub(1));
    }

    fn update_progress_from_line(&mut self, line: &str) {
        let Some(mut progress) = parse_progress_line(line) else {
            return;
        };
        if progress.total.is_none()
            && let Some(previous) = &self.progress
            && previous.phase == progress.phase
        {
            progress.total = previous.total;
            if matches!(progress.status.as_str(), "ok" | "failed")
                && let Some(total) = progress.total
            {
                progress.current = total;
            }
        }
        self.status = match progress.total {
            Some(total) if total > 0 => format!(
                "{} {} {}/{}",
                progress.phase, progress.status, progress.current, total
            ),
            _ => format!("{} {}", progress.phase, progress.status),
        };
        self.progress = Some(progress);
    }
}

fn parse_progress_line(line: &str) -> Option<StoreProgress> {
    let inner = line.strip_prefix("[aicx][")?.strip_suffix(']')?;
    let mut phase = None;
    let mut event = None;
    let mut status = None;
    let mut current = None;
    let mut total = None;

    for part in inner.split_whitespace() {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key {
            "phase" => phase = Some(value.to_string()),
            "event" => event = Some(value.to_string()),
            "status" => status = Some(value.trim_matches('"').to_string()),
            "current" => current = value.parse::<u64>().ok(),
            "total" => total = value.parse::<u64>().ok(),
            _ => {}
        }
    }

    let event = event?;
    let current = match event.as_str() {
        "finish" => total.unwrap_or(current.unwrap_or_default()),
        _ => current.unwrap_or_default(),
    };
    Some(StoreProgress {
        phase: phase?,
        current,
        total,
        status: status.unwrap_or(event),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_progress_tick() {
        let parsed =
            parse_progress_line("[aicx][phase=chunk event=tick elapsed_ms=42 current=7 total=10]")
                .expect("progress");
        assert_eq!(parsed.phase, "chunk");
        assert_eq!(parsed.current, 7);
        assert_eq!(parsed.total, Some(10));
        assert_eq!(parsed.status, "tick");
        assert!((parsed.ratio() - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_structured_progress_finish() {
        let parsed = parse_progress_line(
            "[aicx][phase=steer_sync event=finish status=ok elapsed_ms=9 summary=\"12 docs\"]",
        )
        .expect("progress");
        assert_eq!(parsed.phase, "steer_sync");
        assert_eq!(parsed.current, 0);
        assert_eq!(parsed.total, None);
        assert_eq!(parsed.status, "ok");
    }

    #[test]
    fn ignores_plain_log_lines() {
        assert!(parse_progress_line("  [codex] 12 entries").is_none());
    }
}
