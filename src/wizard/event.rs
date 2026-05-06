use std::io::{Stdout, stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::wizard::{app::App, ui};

type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn run() -> Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new();
    let result = run_app(&mut terminal, &mut app);
    let restore_result = restore_terminal();
    restore_result?;
    result
}

fn init_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    Ok(Terminal::new(backend)?)
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn run_app(terminal: &mut Tui, app: &mut App) -> Result<()> {
    loop {
        app.tick();
        terminal.draw(|frame| ui::render(frame, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key_event(key);
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
