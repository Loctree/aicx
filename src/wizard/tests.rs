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
fn wizard_switches_between_five_screens() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('2'));
    assert_eq!(app.active, Screen::Doctor);
    app.handle_key(KeyCode::Char('3'));
    assert_eq!(app.active, Screen::Intents);
    app.handle_key(KeyCode::Char('4'));
    assert_eq!(app.active, Screen::Rebuild);
    app.handle_key(KeyCode::Char('5'));
    assert_eq!(app.active, Screen::Search);
    app.handle_key(KeyCode::Char('1'));
    assert_eq!(app.active, Screen::Corpus);
}

#[test]
fn wizard_slash_from_any_screen_lands_on_search() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('2'));
    app.handle_key(KeyCode::Char('/'));
    assert_eq!(app.active, Screen::Search);
    assert!(app.search_mode);
}

#[test]
fn wizard_view_search_launch_without_query_opens_input() {
    use super::app::WizardLaunch;
    let app = App::with_launch(WizardLaunch {
        view: Some(Screen::Search),
        query: None,
        project: None,
        agent: None,
    });
    assert_eq!(app.active, Screen::Search);
    assert!(app.search_mode);
}

#[test]
fn screen_parse_accepts_search_aliases() {
    assert_eq!(Screen::parse("search"), Some(Screen::Search));
    assert_eq!(Screen::parse("5"), Some(Screen::Search));
    assert_eq!(Screen::parse("nope"), None);
}

#[test]
fn wizard_store_range_cycles_without_starting_run() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('4'));
    assert_eq!(app.rebuild.hours, 48);
    app.handle_key(KeyCode::Char('t'));
    assert_eq!(app.rebuild.hours, 168);
    assert!(!app.rebuild.running);
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
fn corpus_screen_shows_org_names_and_plain_ask() {
    use std::fs::File;
    use std::io::Write;

    use super::screens::corpus::{CorpusColumn, CorpusItem, CorpusScreen};

    let dir = std::env::temp_dir().join(format!(
        "aicx-wizard-corpus-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"<user_query>\nOperator prompt:\nRepair the corpus column\n</user_query>"}}]}}}}"#
    )
    .unwrap();

    let cwd = "/Users/polyversai/vibecrafted/worktrees/vetcoders/vibecrafted/2026/0909/cursor/work/260910/000212/26389";
    let named = CorpusItem {
        project: "vetcoders/vibecrafted".to_string(),
        stored_project: "vetcoders/vibecrafted".to_string(),
        agent: "cursor".to_string(),
        date: "2026-09-10".to_string(),
        title: "Repair the corpus column".to_string(),
        cwd: "/tmp/vetcoders/vibecrafted".to_string(),
        path: path.clone(),
    };
    let leaked = CorpusItem {
        project: "000212/26389".to_string(),
        stored_project: "000212/26389".to_string(),
        agent: "cursor".to_string(),
        date: "2026-09-10".to_string(),
        title: "You are running under Vibecrafted core runtime.".to_string(),
        cwd: cwd.to_string(),
        path: path.clone(),
    };
    let mut orgs = (0..40)
        .map(|index| CorpusItem {
            project: format!("org-{index:02}/repo"),
            stored_project: format!("org-{index:02}/repo"),
            agent: "cursor".to_string(),
            date: "2026-09-10".to_string(),
            title: "named".to_string(),
            cwd: format!("/tmp/org-{index:02}/repo"),
            path: path.clone(),
        })
        .collect::<Vec<_>>();
    orgs.push(named);
    orgs.push(leaked);
    let mut screen = CorpusScreen::from_items(orgs);
    assert!(screen.stats_line().starts_with("42 sessions"));
    assert_eq!(screen.entries.len(), 42);
    screen.column = CorpusColumn::Orgs;
    screen.org_selected = screen.orgs().len() - 1;

    let app = App::for_corpus_test(screen);
    let backend = TestBackend::new(120, 16);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::render(frame, &app))
        .expect("draw");
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        text.contains("Repair the corpus column"),
        "preview missing ask:\n{text}"
    );
    assert!(!text.contains("000212"), "numeric org leaked:\n{text}");
    assert!(!text.contains("\"type\""), "raw json in preview:\n{text}");
    assert!(
        text.contains("org-39"),
        "org list did not scroll to the last name:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncate_at_boundary_keeps_whole_tokens() {
    assert_eq!(
        ui::truncate_at_boundary("vetcoders/prview-rs", 16),
        "vetcoders…"
    );
    assert_eq!(
        ui::truncate_at_boundary("vibecrafted  2026-09-10  Repair the lock", 28),
        "vibecrafted  2026-09-10…"
    );
    assert_eq!(ui::truncate_at_boundary("loctree", 16), "loctree");
}

#[test]
fn test_paste_with_ansi_escape_treated_as_text() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('/'));
    app.search_input.clear();
    app.handle_paste("\x1b[31mred\x1b[0m".to_string());
    assert_eq!(app.search_input, "[31mred[0m");
}
