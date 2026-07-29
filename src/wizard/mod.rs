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

pub use app::{App, Screen, WizardLaunch};

pub fn run() -> anyhow::Result<()> {
    event::run_with_launch(WizardLaunch::default())
}

pub fn run_with_launch(launch: WizardLaunch) -> anyhow::Result<()> {
    event::run_with_launch(launch)
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
    // Search entry path must boot without stdin prompts.
    let search_app = App::with_launch(WizardLaunch {
        view: Some(Screen::Search),
        query: Some(String::new()),
        project: None,
        agent: None,
    });
    assert_eq!(search_app.active, Screen::Search);
    println!("aicx wizard smoke: booted, rendered, quit, search-view entry ok");
    Ok(())
}
