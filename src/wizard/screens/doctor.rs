use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeverityLabel {
    Green,
    Warning,
    Critical,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct DoctorCard {
    pub name: String,
    pub severity: SeverityLabel,
    pub detail: String,
    pub recommendation: Option<String>,
}

#[derive(Debug, Default)]
pub struct DoctorScreen {
    pub cards: Vec<DoctorCard>,
    pub selected: usize,
    pub loaded: bool,
    pub fixes_applied: Vec<String>,
    pub status: String,
}

impl DoctorScreen {
    pub fn refresh(&mut self, fix: bool) {
        self.run_command(if fix {
            &["doctor", "--fix"]
        } else {
            &["doctor"]
        });
    }

    pub fn fix_buckets(&mut self) {
        self.run_command(&["doctor", "--fix-buckets"]);
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.cards.is_empty() {
            return;
        }
        if delta < 0 {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs());
        } else {
            self.selected = self
                .selected
                .saturating_add(delta as usize)
                .min(self.cards.len() - 1);
        }
    }

    fn run_command(&mut self, args: &[&str]) {
        let Ok(exe) = std::env::current_exe() else {
            self.status = "failed to resolve current aicx executable".to_string();
            self.loaded = true;
            return;
        };

        let output = Command::new(exe)
            .args(args)
            .arg("--format")
            .arg("json")
            .output();
        self.loaded = true;

        let output = match output {
            Ok(output) => output,
            Err(error) => {
                self.status = format!("doctor failed to spawn: {error}");
                return;
            }
        };

        if !output.status.success() && output.stdout.is_empty() {
            self.status = format!(
                "doctor failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return;
        }

        let parsed = serde_json::from_slice::<serde_json::Value>(&output.stdout);
        let Ok(json) = parsed else {
            self.status = format!(
                "doctor output was not json: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return;
        };

        self.cards = [
            "canonical_store",
            "steer_lance",
            "steer_bm25",
            "state",
            "sidecars",
            "corpus_buckets",
        ]
        .into_iter()
        .filter_map(|key| json.get(key).map(card_from_json))
        .collect();
        self.fixes_applied = json
            .get("fixes_applied")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.selected = self.selected.min(self.cards.len().saturating_sub(1));
        self.status = format!(
            "doctor overall: {}",
            json.get("overall")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
        );
    }
}

fn card_from_json(value: &serde_json::Value) -> DoctorCard {
    DoctorCard {
        name: value
            .get("name")
            .and_then(|field| field.as_str())
            .unwrap_or("unknown")
            .to_string(),
        severity: match value
            .get("severity")
            .and_then(|field| field.as_str())
            .unwrap_or("unknown")
        {
            "green" => SeverityLabel::Green,
            "warning" => SeverityLabel::Warning,
            "critical" => SeverityLabel::Critical,
            _ => SeverityLabel::Unknown,
        },
        detail: value
            .get("detail")
            .and_then(|field| field.as_str())
            .unwrap_or("")
            .to_string(),
        recommendation: value
            .get("recommendation")
            .and_then(|field| field.as_str())
            .map(ToOwned::to_owned),
    }
}
