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
fn wizard_store_range_cycles_without_starting_run() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('4'));
    assert_eq!(app.store.hours, 48);
    app.handle_key(KeyCode::Char('t'));
    assert_eq!(app.store.hours, 168);
    assert!(!app.store.running);
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

#[test]
fn test_paste_in_search_mode_appends_text_verbatim() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('/'));
    app.search_input.clear(); // just to be sure
    app.handle_paste("hello world".to_string());
    assert_eq!(app.search_input, "hello world");
}

#[test]
fn test_paste_with_q_does_not_quit() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('/'));
    app.search_input.clear();
    app.handle_paste("q and quit".to_string());
    assert_eq!(app.search_input, "q and quit");
    assert!(!app.should_quit);
}

#[test]
fn test_paste_outside_search_mode_does_not_trigger_quit() {
    let mut app = App::new();
    app.search_mode = false;
    app.handle_paste("q".to_string());
    assert!(!app.should_quit);
}

#[test]
fn test_paste_with_crlf_normalizes_to_lf() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('/'));
    app.search_input.clear();
    app.handle_paste("line1\r\nline2\rline3\nline4".to_string());
    assert_eq!(app.search_input, "line1 line2 line3 line4");
}

#[test]
fn test_paste_with_ansi_escape_treated_as_text() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('/'));
    app.search_input.clear();
    app.handle_paste("\x1b[31mred\x1b[0m".to_string());
    assert_eq!(app.search_input, "[31mred[0m");
}
