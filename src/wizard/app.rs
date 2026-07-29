use crossterm::event::{KeyCode, KeyModifiers};

use crate::wizard::screens::{
    corpus::CorpusScreen, doctor::DoctorScreen, intents::IntentsScreen, rebuild::RebuildScreen,
    search::SearchScreen,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Corpus,
    Doctor,
    Intents,
    Rebuild,
    Search,
}

impl Screen {
    pub fn title(self) -> &'static str {
        match self {
            Self::Corpus => "Corpus",
            Self::Doctor => "Doctor",
            Self::Intents => "Intents",
            Self::Rebuild => "Rebuild",
            Self::Search => "Search",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "corpus" | "1" => Some(Self::Corpus),
            "doctor" | "2" => Some(Self::Doctor),
            "intents" | "3" => Some(Self::Intents),
            "rebuild" | "4" => Some(Self::Rebuild),
            "search" | "5" => Some(Self::Search),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    DoctorFix,
    DoctorFixBuckets,
}

impl Confirmation {
    pub fn command(&self) -> &'static str {
        match self {
            Self::DoctorFix => "aicx doctor --rebuild-steer-index",
            Self::DoctorFixBuckets => "aicx doctor --fix-buckets",
        }
    }
}

/// Optional launch overrides for non-interactive entry (`--view search` etc.).
#[derive(Debug, Clone, Default)]
pub struct WizardLaunch {
    pub view: Option<Screen>,
    pub query: Option<String>,
    pub project: Option<String>,
    pub agent: Option<String>,
}

pub struct App {
    pub active: Screen,
    pub corpus: CorpusScreen,
    pub doctor: DoctorScreen,
    pub intents: IntentsScreen,
    pub rebuild: RebuildScreen,
    pub search: SearchScreen,
    pub should_quit: bool,
    pub show_help: bool,
    pub search_mode: bool,
    pub search_input: String,
    pub confirmation: Option<Confirmation>,
    pub status: String,
}

impl App {
    pub fn new() -> Self {
        Self::with_launch(WizardLaunch::default())
    }

    pub fn with_launch(launch: WizardLaunch) -> Self {
        let active = launch.view.unwrap_or(Screen::Corpus);
        let search = SearchScreen::with_filters(
            launch.query.clone(),
            launch.project.clone(),
            launch.agent.clone(),
        );
        let mut app = Self {
            active,
            corpus: CorpusScreen::load(),
            doctor: DoctorScreen::default(),
            intents: IntentsScreen::load(launch.project.clone(), 168, launch.agent.clone()),
            rebuild: RebuildScreen::default(),
            search,
            should_quit: false,
            show_help: false,
            search_mode: false,
            search_input: launch.query.clone().unwrap_or_default(),
            confirmation: None,
            status: "ready".to_string(),
        };
        if matches!(active, Screen::Search) {
            app.status = app.search.status.clone();
            if app.search.query.is_empty() {
                // Headless entry into search: open the query box without stdin prompts.
                app.search_mode = true;
            }
        } else {
            app.status = app.corpus.status_line();
        }
        app
    }

    pub fn corpus_stats(&self) -> String {
        self.corpus.stats_line()
    }

    pub fn tick(&mut self) {
        self.rebuild.poll();
    }

    pub fn handle_paste(&mut self, content: String) {
        if !self.search_mode {
            return;
        }

        let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
        let sanitized: String = normalized
            .chars()
            .filter(|&c| c >= ' ' || c == '\n' || c == '\t')
            .map(|c| if c == '\n' { ' ' } else { c })
            .collect();

        self.search_input.push_str(&sanitized);
    }

    pub fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.rebuild.cancel() {
                self.status = self.rebuild.status.clone();
            } else {
                self.should_quit = true;
            }
            return;
        }

        self.handle_key(key.code);
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        if self.handle_confirmation_key(key) {
            return;
        }

        if self.show_help {
            match key {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.show_help = false,
                _ => {}
            }
            return;
        }

        if self.search_mode {
            self.handle_search_key(key);
            return;
        }

        match key {
            KeyCode::Char('q') => {
                if self.rebuild.is_running() {
                    self.status = "rebuild is running; press Ctrl+C to cancel or wait".to_string();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Esc => {
                self.confirmation = None;
                self.search_mode = false;
            }
            KeyCode::Char('1') => self.switch(Screen::Corpus),
            KeyCode::Char('2') => self.switch(Screen::Doctor),
            KeyCode::Char('3') => self.switch(Screen::Intents),
            KeyCode::Char('4') => self.switch(Screen::Rebuild),
            KeyCode::Char('5') => self.switch(Screen::Search),
            KeyCode::Char('/') => {
                // Always land on Search for retrieval; corpus/intents keep local filter via Enter routing.
                if !matches!(self.active, Screen::Search) {
                    self.switch(Screen::Search);
                }
                self.search_mode = true;
                self.search_input = if self.search.query.is_empty() {
                    String::new()
                } else {
                    self.search.query.clone()
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Left | KeyCode::Char('h') => self.corpus.move_column(-1),
            KeyCode::Right | KeyCode::Char('l') => self.corpus.move_column(1),
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('f') if self.active == Screen::Doctor => {
                self.confirmation = Some(Confirmation::DoctorFix);
            }
            KeyCode::Char('b') if self.active == Screen::Doctor => {
                self.confirmation = Some(Confirmation::DoctorFixBuckets);
            }
            KeyCode::Char('r') if self.active == Screen::Doctor => {
                self.doctor.refresh(false);
                self.status = self.doctor.status.clone();
            }
            KeyCode::Char('s') if self.active == Screen::Rebuild => {
                self.rebuild.start();
                self.status = self.rebuild.status.clone();
            }
            KeyCode::Char('t') if self.active == Screen::Rebuild => {
                self.rebuild.cycle_hours();
                self.status = self.rebuild.status.clone();
            }
            KeyCode::Char('p') if self.active == Screen::Intents => {
                self.intents.cycle_project_filter();
                self.status = self.intents.status.clone();
            }
            KeyCode::Char('a') if self.active == Screen::Intents => {
                self.intents.cycle_agent_filter();
                self.status = self.intents.status.clone();
            }
            KeyCode::Char('t') if self.active == Screen::Intents => {
                self.intents.cycle_hours();
                self.status = self.intents.status.clone();
            }
            KeyCode::Char('p') if self.active == Screen::Search => {
                self.search.cycle_project_filter();
                self.status = self.search.status.clone();
            }
            KeyCode::Char('a') if self.active == Screen::Search => {
                self.search.cycle_agent_filter();
                self.status = self.search.status.clone();
            }
            KeyCode::Char('t') if self.active == Screen::Search => {
                self.search.cycle_hours();
                self.status = self.search.status.clone();
            }
            KeyCode::Char('r') if self.active == Screen::Search => {
                self.search.refresh_drift();
                if !self.search.query.is_empty() {
                    self.search.run_search();
                }
                self.status = self.search.status.clone();
            }
            _ => {}
        }
    }

    fn switch(&mut self, screen: Screen) {
        self.active = screen;
        self.status = match screen {
            Screen::Corpus => self.corpus.status_line(),
            Screen::Doctor => {
                if !self.doctor.loaded {
                    self.doctor.refresh(false);
                }
                self.doctor.status.clone()
            }
            Screen::Intents => self.intents.status.clone(),
            Screen::Rebuild => self.rebuild.status.clone(),
            Screen::Search => {
                self.search.refresh_drift();
                self.search.status.clone()
            }
        };
    }

    fn move_selection(&mut self, delta: isize) {
        match self.active {
            Screen::Corpus => self.corpus.move_selection(delta),
            Screen::Doctor => self.doctor.move_selection(delta),
            Screen::Intents => self.intents.move_selection(delta),
            Screen::Rebuild => self.rebuild.move_log(delta),
            Screen::Search => self.search.move_selection(delta),
        }
    }

    fn activate_selected(&mut self) {
        match self.active {
            Screen::Corpus => self.status = self.corpus.status_line(),
            Screen::Doctor => {
                self.doctor.refresh(false);
                self.status = self.doctor.status.clone();
            }
            Screen::Intents => self.intents.open_selected(),
            Screen::Rebuild => self.rebuild.start(),
            Screen::Search => {
                self.search.open_selected();
                self.status = "source opened in preview".to_string();
            }
        }
    }

    fn handle_search_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                self.search_mode = false;
                self.search_input.clear();
            }
            KeyCode::Enter => {
                match self.active {
                    Screen::Corpus => {
                        self.corpus.apply_search(self.search_input.clone());
                        self.status = format!("filter: {}", self.search_input);
                    }
                    Screen::Intents => {
                        self.intents.apply_query(self.search_input.clone());
                        self.status = format!("filter: {}", self.search_input);
                    }
                    Screen::Search | Screen::Doctor | Screen::Rebuild => {
                        if !matches!(self.active, Screen::Search) {
                            self.switch(Screen::Search);
                        }
                        self.search.apply_query(self.search_input.clone());
                        self.status = self.search.status.clone();
                    }
                }
                self.search_mode = false;
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn handle_confirmation_key(&mut self, key: KeyCode) -> bool {
        let Some(action) = self.confirmation.clone() else {
            return false;
        };

        match key {
            KeyCode::Char('y') | KeyCode::Enter => {
                match action {
                    Confirmation::DoctorFix => self.doctor.refresh(true),
                    Confirmation::DoctorFixBuckets => self.doctor.fix_buckets(),
                }
                self.status = self.doctor.status.clone();
                self.confirmation = None;
                true
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.confirmation = None;
                self.status = "action cancelled".to_string();
                true
            }
            _ => true,
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
