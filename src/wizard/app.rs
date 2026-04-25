use crossterm::event::{KeyCode, KeyModifiers};

use crate::wizard::screens::{
    corpus::CorpusScreen, doctor::DoctorScreen, intents::IntentsScreen, store::StoreScreen,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Corpus,
    Doctor,
    Intents,
    Store,
}

impl Screen {
    pub fn title(self) -> &'static str {
        match self {
            Self::Corpus => "Corpus",
            Self::Doctor => "Doctor",
            Self::Intents => "Intents",
            Self::Store => "Store",
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
            Self::DoctorFix => "aicx doctor --fix",
            Self::DoctorFixBuckets => "aicx doctor --fix-buckets",
        }
    }
}

pub struct App {
    pub active: Screen,
    pub corpus: CorpusScreen,
    pub doctor: DoctorScreen,
    pub intents: IntentsScreen,
    pub store: StoreScreen,
    pub should_quit: bool,
    pub show_help: bool,
    pub search_mode: bool,
    pub search_input: String,
    pub confirmation: Option<Confirmation>,
    pub status: String,
}

impl App {
    pub fn new() -> Self {
        let mut app = Self {
            active: Screen::Corpus,
            corpus: CorpusScreen::load(),
            doctor: DoctorScreen::default(),
            intents: IntentsScreen::load(None, 168, None),
            store: StoreScreen::default(),
            should_quit: false,
            show_help: false,
            search_mode: false,
            search_input: String::new(),
            confirmation: None,
            status: "ready".to_string(),
        };
        app.status = app.corpus.status_line();
        app
    }

    pub fn corpus_stats(&self) -> String {
        self.corpus.stats_line()
    }

    pub fn tick(&mut self) {
        self.store.poll();
    }

    pub fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.store.cancel() {
                self.status = "store run cancelled".to_string();
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
                if self.store.is_running() {
                    self.status = "store is running; press Ctrl+C to cancel or wait".to_string();
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
            KeyCode::Char('4') => self.switch(Screen::Store),
            KeyCode::Char('/') => {
                self.search_mode = true;
                self.search_input = match self.active {
                    Screen::Corpus => self.corpus.search.clone(),
                    Screen::Intents => self.intents.query.clone(),
                    _ => String::new(),
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
            KeyCode::Char('s') if self.active == Screen::Store => {
                self.store.start();
                self.status = self.store.status.clone();
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
            Screen::Store => self.store.status.clone(),
        };
    }

    fn move_selection(&mut self, delta: isize) {
        match self.active {
            Screen::Corpus => self.corpus.move_selection(delta),
            Screen::Doctor => self.doctor.move_selection(delta),
            Screen::Intents => self.intents.move_selection(delta),
            Screen::Store => self.store.move_log(delta),
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
            Screen::Store => self.store.start(),
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
                    Screen::Corpus => self.corpus.apply_search(self.search_input.clone()),
                    Screen::Intents => self.intents.apply_query(self.search_input.clone()),
                    _ => {}
                }
                self.search_mode = false;
                self.status = format!("filter: {}", self.search_input);
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
