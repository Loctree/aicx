use crossterm::event::KeyCode;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::{App, Screen, ui};

#[test]
fn wizard_q_quits_when_idle() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('q'));
    assert!(app.should_quit);
}

#[test]
fn wizard_switches_between_four_screens() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('2'));
    assert_eq!(app.active, Screen::Doctor);
    app.handle_key(KeyCode::Char('3'));
    assert_eq!(app.active, Screen::Intents);
    app.handle_key(KeyCode::Char('4'));
    assert_eq!(app.active, Screen::Store);
    app.handle_key(KeyCode::Char('1'));
    assert_eq!(app.active, Screen::Corpus);
}

#[test]
fn wizard_renders_to_test_backend() {
    let app = App::new();
    let backend = TestBackend::new(100, 32);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::render(frame, &app))
        .expect("draw");
}
