use std::fs;

use crate::intents::{self, IntentDisplayFilters, IntentRecord, IntentSortOrder};

#[derive(Debug)]
pub struct IntentsScreen {
    pub records: Vec<IntentRecord>,
    pub visible: Vec<IntentRecord>,
    pub selected: usize,
    pub query: String,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub hours: u64,
    pub preview: String,
    pub status: String,
}

impl IntentsScreen {
    pub fn load(project: Option<String>, hours: u64, agent: Option<String>) -> Self {
        let project_filter = project.clone().unwrap_or_else(|| "aicx".to_string());
        let config = intents::IntentsConfig {
            project: project_filter.clone(),
            hours,
            strict: false,
            kind_filter: None,
            frame_kind: None,
        };

        match intents::extract_intents(&config) {
            Ok(records) => {
                let mut screen = Self {
                    records,
                    visible: Vec::new(),
                    selected: 0,
                    query: String::new(),
                    project: project,
                    agent,
                    hours,
                    preview: String::new(),
                    status: String::new(),
                };
                screen.apply_filters();
                screen.status = format!(
                    "{} intents loaded for project filter '{}'",
                    screen.visible.len(),
                    project_filter
                );
                screen
            }
            Err(error) => Self {
                records: Vec::new(),
                visible: Vec::new(),
                selected: 0,
                query: String::new(),
                project,
                agent,
                hours,
                preview: String::new(),
                status: format!("intent load failed: {error}"),
            },
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        if delta < 0 {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs());
        } else {
            self.selected = self
                .selected
                .saturating_add(delta as usize)
                .min(self.visible.len() - 1);
        }
    }

    pub fn apply_query(&mut self, query: String) {
        self.query = query.trim().to_string();
        self.apply_filters();
    }

    pub fn open_selected(&mut self) {
        let Some(record) = self.visible.get(self.selected) else {
            self.preview = "No intent selected.".to_string();
            return;
        };
        self.preview = match fs::read_to_string(&record.source_chunk) {
            Ok(raw) => raw.lines().take(80).collect::<Vec<_>>().join("\n"),
            Err(error) => format!("Failed to open {}: {error}", record.source_chunk),
        };
    }

    pub fn selected_preview(&self) -> String {
        if !self.preview.is_empty() {
            return self.preview.clone();
        }
        self.visible
            .get(self.selected)
            .map(|record| {
                format!(
                    "{} | {} | {}\n{}\n\nsource: {}",
                    record.date,
                    record.agent,
                    record.kind.heading(),
                    record.summary,
                    record.source_chunk
                )
            })
            .unwrap_or_else(|| "No intents visible.".to_string())
    }

    pub fn cycle_project_filter(&mut self) {
        self.project = match self.project.as_deref() {
            None => Some("aicx".to_string()),
            Some("aicx") => Some("memex".to_string()),
            Some(_) => None,
        };
        *self = Self::load(self.project.clone(), self.hours, self.agent.clone());
    }

    pub fn cycle_agent_filter(&mut self) {
        self.agent = match self.agent.as_deref() {
            None => Some("codex".to_string()),
            Some("codex") => Some("claude".to_string()),
            Some(_) => None,
        };
        self.apply_filters();
    }

    pub fn cycle_hours(&mut self) {
        self.hours = match self.hours {
            48 => 168,
            168 => 720,
            _ => 48,
        };
        *self = Self::load(self.project.clone(), self.hours, self.agent.clone());
    }

    fn apply_filters(&mut self) {
        let filters = IntentDisplayFilters {
            agent: self.agent.clone(),
            sort: Some(IntentSortOrder::Newest),
            limit: Some(200),
            ..IntentDisplayFilters::default()
        };
        let mut visible = intents::apply_display_filters(self.records.clone(), &filters);
        if !self.query.is_empty() {
            let needle = self.query.to_ascii_lowercase();
            visible.retain(|record| {
                format!(
                    "{} {} {} {} {}",
                    record.summary,
                    record.context.clone().unwrap_or_default(),
                    record.agent,
                    record.project,
                    record.source_chunk
                )
                .to_ascii_lowercase()
                .contains(&needle)
            });
        }
        self.visible = visible;
        self.selected = 0;
        self.preview.clear();
        self.status = format!("{} intents visible", self.visible.len());
    }
}
