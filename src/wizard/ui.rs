use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::wizard::app::{App, Confirmation, Screen};
use crate::wizard::screens::corpus::CorpusColumn;
use crate::wizard::screens::doctor::SeverityLabel;

pub fn render(frame: &mut Frame, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_topbar(frame, layout[0], app);
    render_main(frame, layout[1], app);
    render_bottombar(frame, layout[2], app);

    if app.show_help {
        render_help(frame, centered_rect(64, 46, frame.area()));
    }
    if app.search_mode {
        render_search(frame, centered_rect(60, 18, frame.area()), app);
    }
    if let Some(action) = &app.confirmation {
        render_confirmation(frame, centered_rect(64, 24, frame.area()), action);
    }
}

fn render_topbar(frame: &mut Frame, area: Rect, app: &App) {
    let text = format!(
        " aicx wizard | {} | {} ",
        app.active.title(),
        app.corpus_stats()
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        area,
    );
}

fn render_bottombar(frame: &mut Frame, area: Rect, app: &App) {
    let text = match app.active {
        Screen::Corpus => " q quit | 1-5 screens | / search | hjkl nav | Enter preview | ? help ",
        Screen::Doctor => " q quit | r refresh | f fix steer | b fix buckets | / search | ? help ",
        Screen::Intents => {
            " q quit | p project | a agent | t time | / search | Enter chunk | ? help "
        }
        Screen::Rebuild => {
            " q quit | s start | t range | Ctrl+C cancel | / search | jk scroll | ? help "
        }
        Screen::Search => {
            " q quit | / query | Enter run/open | p project | a agent | t time | r re-run | ? help "
        }
    };
    let line = format!("{} | {}", text, app.status);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::Black).bg(Color::Gray)),
        area,
    );
}

fn render_main(frame: &mut Frame, area: Rect, app: &App) {
    match app.active {
        Screen::Corpus => render_corpus(frame, area, app),
        Screen::Doctor => render_doctor(frame, area, app),
        Screen::Intents => render_intents(frame, area, app),
        Screen::Rebuild => render_store(frame, area, app),
        Screen::Search => render_search_view(frame, area, app),
    }
}

fn render_search_view(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(if app.search.drift_banner.is_some() {
                4
            } else {
                2
            }),
        ])
        .split(area);

    let meta = format!(
        "q={} · p={} · a={} · t={} · {}",
        if app.search.query.is_empty() {
            "∅".to_string()
        } else {
            truncate(&app.search.query, 40)
        },
        app.search.project.as_deref().unwrap_or("*"),
        app.search.agent.as_deref().unwrap_or("*"),
        if app.search.hours == 0 {
            "all".to_string()
        } else {
            format!("{}h", app.search.hours)
        },
        app.search.backend_line
    );
    frame.render_widget(
        Paragraph::new(meta)
            .block(block("Search engine (aicx search)"))
            .wrap(Wrap { trim: true }),
        chunks[0],
    );

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(chunks[1]);

    let items = app
        .search
        .hits
        .iter()
        .enumerate()
        .map(|(idx, hit)| {
            let style = if idx == app.search.selected {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!(
                    "{}{}",
                    if idx == app.search.selected {
                        "> "
                    } else {
                        "  "
                    },
                    truncate(&hit.list_label(), 90)
                ),
                style,
            )))
        })
        .collect::<Vec<_>>();
    let title = if app.search.hits.is_empty() {
        "Hits (empty)"
    } else {
        "Hits"
    };
    frame.render_widget(List::new(items).block(block(title)), body[0]);

    frame.render_widget(
        Paragraph::new(app.search.selected_preview())
            .block(block("Source"))
            .wrap(Wrap { trim: false }),
        body[1],
    );

    let footer = if let Some(banner) = &app.search.drift_banner {
        let repair = app
            .search
            .repair_hint
            .as_deref()
            .unwrap_or("aicx catalog rebuild && aicx index");
        format!("{banner}\n→ {repair}")
    } else {
        app.search.backend_line.clone()
    };
    let style = if app.search.drift_banner.is_some() {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    };
    frame.render_widget(
        Paragraph::new(footer)
            .style(style)
            .block(block("Truth / drift"))
            .wrap(Wrap { trim: true }),
        chunks[2],
    );
}

fn render_corpus(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(18),
            Constraint::Percentage(24),
            Constraint::Percentage(30),
            Constraint::Percentage(28),
        ])
        .split(area);

    render_simple_list(
        frame,
        chunks[0],
        "Orgs",
        app.corpus.orgs(),
        app.corpus.column == CorpusColumn::Orgs,
        0,
    );
    render_simple_list(
        frame,
        chunks[1],
        "Repos",
        app.corpus.repos(),
        app.corpus.column == CorpusColumn::Repos,
        0,
    );

    let chunk_items = app
        .corpus
        .entries
        .iter()
        .map(|entry| entry.label.clone())
        .collect::<Vec<_>>();
    render_simple_list(
        frame,
        chunks[2],
        "Chunks",
        chunk_items,
        app.corpus.column == CorpusColumn::Chunks,
        app.corpus.selected,
    );

    let preview = app.corpus.selected_preview();
    frame.render_widget(
        Paragraph::new(preview)
            .block(block("Preview"))
            .wrap(Wrap { trim: false }),
        chunks[3],
    );
}

fn render_doctor(frame: &mut Frame, area: Rect, app: &App) {
    if !app.doctor.loaded {
        frame.render_widget(
            Paragraph::new("Press r or Enter to run aicx doctor.").block(block("Doctor")),
            area,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);

    let cards = app
        .doctor
        .cards
        .iter()
        .enumerate()
        .map(|(idx, card)| {
            let style = severity_style(card.severity.clone());
            let marker = if idx == app.doctor.selected {
                "> "
            } else {
                "  "
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(format!("{:?}", card.severity), style),
                Span::raw(format!(" {}", card.name)),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(cards).block(block("Checks")), chunks[0]);

    let detail = app
        .doctor
        .cards
        .get(app.doctor.selected)
        .map(|card| {
            format!(
                "{}\n\n{}\n\n{}",
                card.name,
                card.detail,
                card.recommendation
                    .clone()
                    .unwrap_or_else(|| "No recommendation.".to_string())
            )
        })
        .unwrap_or_else(|| app.doctor.status.clone());
    frame.render_widget(
        Paragraph::new(detail)
            .block(block("Selected"))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );
}

fn render_intents(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(area);

    let items = app
        .intents
        .visible
        .iter()
        .enumerate()
        .map(|(idx, record)| {
            let style = if idx == app.intents.selected {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    if idx == app.intents.selected {
                        "> "
                    } else {
                        "  "
                    },
                    style,
                ),
                Span::styled(record.kind.heading(), Style::default().fg(Color::Yellow)),
                Span::raw(format!(
                    " {} {} {}",
                    record.date,
                    record.agent,
                    truncate(&record.summary, 72)
                )),
            ]))
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        List::new(items).block(block(&format!(
            "Timeline - {}h - agent {:?}",
            app.intents.hours, app.intents.agent
        ))),
        chunks[0],
    );

    frame.render_widget(
        Paragraph::new(app.intents.selected_preview())
            .block(block("Source"))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );
}

fn render_store(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8)])
        .split(area);

    let ratio = app
        .rebuild
        .progress
        .as_ref()
        .map(|progress| progress.ratio())
        .unwrap_or(0.0);
    let label = match (app.rebuild.running, app.rebuild.progress.as_ref()) {
        (true, Some(progress)) => match progress.total {
            Some(total) if total > 0 => format!(
                "{} {} {}/{} - last {}h",
                progress.phase, progress.status, progress.current, total, app.rebuild.hours
            ),
            _ => format!(
                "{} {} - last {}h",
                progress.phase, progress.status, app.rebuild.hours
            ),
        },
        (true, None) => format!("rebuild starting - last {}h", app.rebuild.hours),
        (false, Some(progress)) => format!("last phase: {} {}", progress.phase, progress.status),
        (false, None) => format!("idle - next run last {}h", app.rebuild.hours),
    };
    frame.render_widget(
        Gauge::default()
            .block(block("Rebuild"))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(label.as_str()),
        chunks[0],
    );

    let start = app.rebuild.scroll.min(app.rebuild.log.len());
    let lines = app.rebuild.log[start..]
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(block("Log Tail"))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );
}

fn render_simple_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    values: Vec<String>,
    focused: bool,
    selected: usize,
) {
    let items = values
        .into_iter()
        .take(200)
        .enumerate()
        .map(|(idx, value)| {
            let style = if focused && idx == selected {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(truncate(&value, 80), style)))
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items).block(block(title)), area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let text = vec![
        Line::from("aicx wizard keymap"),
        Line::from(""),
        Line::from("1 corpus | 2 doctor | 3 intents | 4 rebuild | 5 search"),
        Line::from("hjkl / arrows navigate visible lists"),
        Line::from("/ opens search query (from any screen → Search view)"),
        Line::from("search: Enter runs aicx search; p/a/t filters; r re-run; Enter opens source"),
        Line::from(
            "doctor: r refresh, f runs aicx doctor --rebuild-steer-index, b shows Plan B deferral",
        ),
        Line::from("rebuild: s runs aicx catalog rebuild && aicx index --cache-extracts"),
        Line::from("rebuild: Ctrl+C cancels a long rebuild when supported"),
        Line::from("CLI: aicx wizard --view search --query '…' [--project p] [--agent a]"),
        Line::from("q quits when no long operation is in flight"),
    ];
    frame.render_widget(Paragraph::new(text).block(block("Help")), area);
}

fn render_search(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Clear, area);
    let title = if matches!(app.active, Screen::Search) {
        "Search query (Enter runs CURRENT; Esc cancel)"
    } else {
        "Filter / search (Enter applies)"
    };
    frame.render_widget(
        Paragraph::new(format!("> {}", app.search_input)).block(block(title)),
        area,
    );
}

fn render_confirmation(frame: &mut Frame, area: Rect, action: &Confirmation) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!(
            "Run this command?\n\n{}\n\nEnter/y confirms, Esc/n cancels.",
            action.command()
        ))
        .block(block("Confirm")),
        area,
    );
}

fn block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title.to_string())
}

fn severity_style(severity: SeverityLabel) -> Style {
    match severity {
        SeverityLabel::Green => Style::default().fg(Color::Green),
        SeverityLabel::Warning => Style::default().fg(Color::Yellow),
        SeverityLabel::Critical => Style::default().fg(Color::Red).bold(),
        SeverityLabel::Unknown => Style::default().fg(Color::Gray),
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}
