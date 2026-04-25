use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

#[derive(Debug)]
enum StoreEvent {
    Line(String),
    Done(bool),
}

#[derive(Debug, Default)]
pub struct StoreScreen {
    pub running: bool,
    pub log: Vec<String>,
    pub scroll: usize,
    pub status: String,
    rx: Option<mpsc::Receiver<StoreEvent>>,
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

        let Ok(exe) = std::env::current_exe() else {
            self.status = "failed to resolve current aicx executable".to_string();
            return;
        };

        let (event_tx, event_rx) = mpsc::channel();
        self.rx = Some(event_rx);
        self.running = true;
        self.log.clear();
        self.log
            .push("running: aicx store -H 48 --emit none".to_string());
        self.status = "store run started".to_string();

        thread::spawn(move || {
            let mut child = match Command::new(exe)
                .arg("store")
                .arg("-H")
                .arg("48")
                .arg("--emit")
                .arg("none")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    let _ = event_tx.send(StoreEvent::Line(format!("spawn failed: {error}")));
                    let _ = event_tx.send(StoreEvent::Done(false));
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

            let ok = child.wait().map(|status| status.success()).unwrap_or(false);
            let _ = event_tx.send(StoreEvent::Done(ok));
        });
    }

    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        while let Ok(event) = rx.try_recv() {
            match event {
                StoreEvent::Line(line) => self.log.push(line),
                StoreEvent::Done(ok) => {
                    self.running = false;
                    self.status = if ok {
                        "store run completed".to_string()
                    } else {
                        "store run failed".to_string()
                    };
                }
            }
        }
    }

    pub fn cancel(&mut self) -> bool {
        if !self.running {
            return false;
        }
        self.running = false;
        self.log
            .push("cancel requested; subprocess fallback cannot guarantee termination".to_string());
        true
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
}
