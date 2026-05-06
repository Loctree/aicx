//! Interactive `aicx wizard` entrypoint.
//!
//! The wizard is intentionally additive: all existing CLI commands remain the
//! scripting surface, while this module gives operators a full-screen daily
//! driver over the same library contracts.

pub mod app;
mod event;
pub mod screens;
pub mod ui;

#[cfg(test)]
mod tests;

pub use app::{App, Screen};

pub fn run() -> anyhow::Result<()> {
    event::run()
}

pub fn smoke_test() -> anyhow::Result<()> {
    let mut app = App::new();
    app.handle_key(crossterm::event::KeyCode::Char('q'));
    let backend = ratatui::backend::TestBackend::new(100, 32);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.draw(|frame| ui::render(frame, &app))?;
    if !app.should_quit {
        anyhow::bail!("wizard smoke did not set quit state");
    }
    println!("aicx wizard smoke: booted, rendered, quit");
    Ok(())
}
