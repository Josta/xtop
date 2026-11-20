use crate::{
    model::{App, Key, View, bytes, classify, display_home},
    procfs::{self, Snapshot},
};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, Wrap},
};
use std::{io, sync::mpsc::Receiver, time::Duration};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
// ANSI Gray is palette slot 7, often identical to the default text color.
const CWD_COLOR: Color = Color::Rgb(128, 140, 156);

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

pub fn run(mut app: App, rx: Receiver<Result<Snapshot, String>>) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = Restore;
    if app.config.mouse {
        execute!(io::stdout(), EnableMouseCapture)?;
    }
    let mut table_area = Rect::default();
    let mut saved_preferences = crate::preferences::Preferences::from(&app);
    loop {
        crate::preferences::save_if_changed(&mut app, &mut saved_preferences);
        while let Ok(result) = rx.try_recv() {
            match result {
                Ok(s) => app.update(s),
                Err(e) => app.status = e,
            }
        }
        terminal.draw(|f| table_area = draw(f, &mut app))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if app.confirmation.is_some() {
                    match key.code {
                        KeyCode::Down | KeyCode::Char('j') => {
                            let count = app.confirmation.as_ref().unwrap().0.len();
                            app.confirmation_offset =
                                (app.confirmation_offset + 1).min(count.saturating_sub(1));
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.confirmation_offset = app.confirmation_offset.saturating_sub(1)
                        }
                        KeyCode::Esc | KeyCode::Char('n') => app.confirmation = None,
                        KeyCode::Char('y') => {
                            let (targets, signal, _) = app.confirmation.take().unwrap();
                            let mut ok = 0;
                            let mut errors = vec![];
                            for target in targets {
                                match procfs::signal(target, signal) {
                                    Ok(()) => ok += 1,
                                    Err(e) => errors.push(format!("{}: {e}", target.pid)),
                                }
                            }
                            app.status = format!("Signaled {ok} processes. {}", errors.join("; "));
                        }
                        _ => {}
                    }
                    continue;
                }
                if app.help {
                    if matches!(
                        key.code,
                        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                    ) {
                        app.help = false;
                    }
                    continue;
                }
                if app.settings {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('o') | KeyCode::Char('q') => {
                            app.settings = false
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.settings_cursor = (app.settings_cursor + 1) % 11
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.settings_cursor = (app.settings_cursor + 10) % 11
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => toggle_setting(&mut app),
                        KeyCode::Char('m') => app.cycle_measurement(),
                        _ => {}
                    }
                    continue;
                }
                if app.editing_filter {
                    match key.code {
                        KeyCode::Esc => {
                            app.editing_filter = false;
                            app.filter.clear();
                            app.rebuild();
                        }
                        KeyCode::Enter => app.editing_filter = false,
                        KeyCode::Backspace => {
                            app.filter.pop();
                            app.rebuild();
                        }
                        KeyCode::Char(c) => {
                            app.filter.push(c);
                            app.rebuild();
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('?') | KeyCode::F(1) => app.help = true,
                    KeyCode::Char('o') | KeyCode::F(2) => app.settings = true,
                    KeyCode::Down | KeyCode::Char('j') => app.navigate(1),
                    KeyCode::Up | KeyCode::Char('k') => app.navigate(-1),
                    KeyCode::PageDown => app.navigate(app.viewport as isize),
                    KeyCode::PageUp => app.navigate(-(app.viewport as isize)),
                    KeyCode::Home => app.navigate(-(app.rows.len() as isize)),
                    KeyCode::End => app.navigate(app.rows.len() as isize),
                    KeyCode::Enter | KeyCode::Char(' ') => app.toggle(),
                    KeyCode::Right | KeyCode::Char('l') => {
                        if app.current().is_some_and(|r| !r.open) {
                            app.toggle();
                        } else {
                            app.navigate(1);
                        }
                    }
                    KeyCode::Left | KeyCode::Char('h') => app.collapse(),
                    KeyCode::Tab => app.change_view(),
                    KeyCode::Char('s') => {
                        app.sort = app.sort.next();
                        app.rebuild();
                    }
                    KeyCode::Char('/') => {
                        app.editing_filter = true;
                        app.depth_limit = None;
                    }
                    KeyCode::Char('m') => app.cycle_measurement(),
                    KeyCode::Char('S') => app.toggle_system(),
                    KeyCode::Char('H') => app.toggle_workload_tree(),
                    KeyCode::Char(c @ '1'..='9') => app.set_depth(c.to_digit(10).unwrap() as usize),
                    KeyCode::Esc => {
                        app.filter.clear();
                        app.rebuild();
                    }
                    KeyCode::Char('p') => app.paused = !app.paused,
                    KeyCode::Char('d') => app.details = !app.details,
                    KeyCode::Char('t') => app.prepare_signal(libc::SIGTERM, false),
                    KeyCode::Char('K') => app.prepare_signal(libc::SIGTERM, true),
                    KeyCode::Char('x') => app.prepare_signal(libc::SIGKILL, false),
                    KeyCode::Char('e') => {
                        app.depth_limit = None;
                        for row in &app.rows {
                            if row.expandable {
                                app.expanded.insert(row.key.clone());
                            }
                        }
                        app.rebuild();
                    }
                    KeyCode::Char('c') => app.set_depth(1),
                    _ => {}
                }
            }
            Event::Mouse(mouse) if !app.help && !app.settings && app.confirmation.is_none() => {
                match mouse.kind {
                    MouseEventKind::ScrollDown => app.navigate(3),
                    MouseEventKind::ScrollUp => app.navigate(-3),
                    MouseEventKind::Down(MouseButton::Left)
                        if mouse.column >= table_area.x
                            && mouse.column < table_area.right()
                            && mouse.row > table_area.y + 1
                            && mouse.row < table_area.bottom().saturating_sub(1) =>
                    {
                        let i = app.offset + (mouse.row - table_area.y - 2) as usize;
                        if let Some(row) = app.rows.get(i) {
                            let same = app.selected.as_ref() == Some(&row.key);
                            app.selected = Some(row.key.clone());
                            if same {
                                app.toggle();
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    crate::preferences::save_if_changed(&mut app, &mut saved_preferences);
    Ok(())
}

fn toggle_setting(app: &mut App) {
    match app.settings_cursor {
        0 => app.show_pid = !app.show_pid,
        1 => app.show_cpu = !app.show_cpu,
        2 => app.show_pss = !app.show_pss,
        3 => app.show_swap = !app.show_swap,
        4 => app.show_total = !app.show_total,
        5 => app.details = !app.details,
        6 => {
            app.config.mouse = !app.config.mouse;
            if app.config.mouse {
                let _ = execute!(io::stdout(), EnableMouseCapture);
            } else {
                let _ = execute!(io::stdout(), DisableMouseCapture);
            }
        }
        7 => app.toggle_system(),
        8 => app.toggle_workload_tree(),
        9 => app.show_age = !app.show_age,
        10 => app.cycle_measurement(),
        _ => {}
    }
}

pub fn draw(f: &mut Frame, app: &mut App) -> Rect {
    let area = f.area();
    if area.width < 48 || area.height < 12 {
        f.render_widget(
            Paragraph::new(
                "xtop needs at least 48 columns × 12 rows. Resize the terminal; q quits.",
            )
            .wrap(Wrap { trim: true }),
            area,
        );
        return Rect::default();
    }
    let detail_height = if app.details && area.height >= 24 {
        10
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(detail_height),
            Constraint::Length(2),
        ])
        .split(area);
    let title = format!(
        " xtop  m: {}   {}{}   s: sort {}   S: system {}{}",
        app.measurement.label(),
        app.view.name(),
        if app.view == View::Groups {
            if app.workload_tree {
                " [tree]"
            } else {
                " [flat]"
            }
        } else {
            ""
        },
        app.sort.name(),
        if app.show_system { "shown" } else { "hidden" },
        if app.paused { "   PAUSED" } else { "" }
    );
    f.render_widget(
        Paragraph::new(title).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    summary(f, chunks[1], app);
    let mut table = chunks[2];
    app.viewport = table.height.saturating_sub(3) as usize;
    app.ensure_visible();
    let compact = area.width < 90;
    let mut widths = vec![Constraint::Min(24)];
    let mut headers = vec!["WORKLOAD / PROCESS".to_string()];
    if app.show_cpu {
        widths.push(Constraint::Length(9));
        headers.push("CPU".into());
    }
    if app.show_pss {
        widths.push(Constraint::Length(9));
        headers.push("RAM".into());
    }
    if app.show_swap && !compact {
        widths.push(Constraint::Length(9));
        headers.push("SWAP".into());
    }
    if app.show_total && !compact {
        widths.push(Constraint::Length(9));
        headers.push("Footprint".into());
    }
    if app.show_age && !compact {
        widths.push(Constraint::Length(7));
        headers.push("AGE".into());
    }
    if app.show_pid && !compact {
        widths.push(Constraint::Length(8));
        headers.push("PID".into());
    }
    let mut name_width = 24usize;
    let mut previous_values: [Option<String>; 4] = Default::default();
    let rows: Vec<_> = app
        .rows
        .iter()
        .skip(app.offset)
        .take(app.viewport)
        .map(|r| {
            let arrow = if !r.expandable {
                " "
            } else if r.open {
                "▾"
            } else {
                "▸"
            };
            let name = if app.view == View::Cgroups && matches!(r.key, Key::Group(_)) {
                r.label.rsplit('/').next().unwrap_or(&r.label)
            } else {
                &r.label
            };
            let process = match r.key {
                Key::Process(id) => app.snapshot.as_ref().and_then(|s| s.processes.get(&id.pid)),
                _ => None,
            };
            let child = r
                .folded
                .last()
                .and_then(|id| app.snapshot.as_ref()?.processes.get(&id.pid));
            let title = if child.is_some() {
                let mut names = process
                    .map(|p| vec![crate::model::process_name(p)])
                    .unwrap_or_default();
                if let Some(s) = &app.snapshot {
                    names.extend(
                        r.folded
                            .iter()
                            .filter_map(|id| s.processes.get(&id.pid))
                            .map(crate::model::process_name),
                    );
                }
                names.join(": ")
            } else {
                process.map_or(name, crate::model::process_name).to_string()
            };
            let mut spans = vec![Span::raw(format!(
                "{}{} {}",
                "  ".repeat(r.depth.min(12)),
                arrow,
                title
            ))];
            if let Some(p) = child.or(process)
                && !p.cwd.is_empty()
            {
                spans.push(Span::styled(
                    format!("  {}", display_home(&p.cwd)),
                    Style::default()
                        .fg(CWD_COLOR)
                        .remove_modifier(Modifier::BOLD),
                ));
            }
            let name_line = Line::from(spans);
            name_width = name_width.max(name_line.width());
            let mut cells = vec![Cell::from(name_line)];
            for (index, enabled, metric) in [
                (0, app.show_cpu, crate::history::Metric::Cpu),
                (1, app.show_pss, crate::history::Metric::Ram),
                (2, app.show_swap && !compact, crate::history::Metric::Swap),
                (
                    3,
                    app.show_total && !compact,
                    crate::history::Metric::Footprint,
                ),
            ] {
                if !enabled {
                    continue;
                }
                let (value, partial) = app.measured(r, metric);
                let m = crate::model::Metrics {
                    count: if partial { 2 } else { 1 },
                    covered: usize::from(value.is_some()),
                    cpu_covered: usize::from(value.is_some()),
                    cpu: value.unwrap_or(0.0),
                    pss: value.unwrap_or(0.0).round() as u64,
                    ..Default::default()
                };
                let text = if index == 0 {
                    m.cpu_text()
                } else {
                    m.pss_text()
                };
                let text = ditto(text, &mut previous_values[index]);
                cells.push(if index == 0 {
                    cpu_cell(text, &m)
                } else {
                    memory_cell(text, m.pss, m.covered)
                });
            }
            if app.show_age && !compact {
                cells.push(Cell::from(
                    child.or(process).map_or(String::new(), |p| age_text(p.age)),
                ));
            }
            if app.show_pid && !compact {
                cells.push(Cell::from(match r.key {
                    Key::Process(_) if !r.folded.is_empty() => {
                        r.folded.last().unwrap().pid.to_string()
                    }
                    Key::Process(id) => id.pid.to_string(),
                    Key::Group(_) => String::new(),
                }));
            }
            let style = if app.selected.as_ref() == Some(&r.key) {
                Style::default()
                    .bg(Color::Rgb(35, 58, 74))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if matches!(r.key, Key::Group(_)) {
                Style::default().fg(ACCENT)
            } else {
                Style::default()
            };
            Row::new(cells).style(style)
        })
        .collect();
    // Fit the table to visible names rather than stretching the name column
    // across the terminal. Borders and one-cell column gaps count toward width.
    let fixed_width: usize = widths
        .iter()
        .skip(1)
        .map(|w| match w {
            Constraint::Length(n) => usize::from(*n),
            _ => 0,
        })
        .sum();
    let desired = (name_width + fixed_width + widths.len() - 1 + 2).max(80);
    table.width = desired.min(usize::from(table.width)) as u16;
    let coverage = app
        .snapshot
        .as_ref()
        .map(|s| {
            format!(
                "PSS {}/{} · * partial",
                s.processes.values().filter(|p| p.memory.is_some()).count(),
                s.processes.len()
            )
        })
        .unwrap_or_else(|| "Collecting…".into());
    let caption = format!(
        " {} · {}–{}/{} · {} ",
        app.view.name(),
        if app.rows.is_empty() {
            0
        } else {
            app.offset + 1
        },
        (app.offset + app.viewport).min(app.rows.len()),
        app.rows.len(),
        coverage
    );
    f.render_widget(
        Table::new(rows, widths)
            .column_spacing(1)
            .header(
                Row::new(headers.into_iter().map(|header| {
                    let memory = matches!(header.as_str(), "CPU" | "RAM" | "SWAP" | "Footprint");
                    let line = Line::from(header);
                    Cell::from(if memory { line.right_aligned() } else { line })
                }))
                .style(Style::default().fg(MUTED)),
            )
            .block(Block::default().borders(Borders::ALL).title(caption)),
        table,
    );
    if detail_height > 0 {
        details(f, chunks[3], app);
    }
    let filter = if app.editing_filter || !app.filter.is_empty() {
        format!(
            " / {}{}",
            app.filter,
            if app.editing_filter { "▏" } else { "" }
        )
    } else if !app.status.is_empty() {
        format!(" {}", display_home(&app.status))
    } else {
        " ↑↓ select · Enter expand · / find · t terminate · K kill group/tree · p pause".into()
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::raw(filter),
            Line::styled(
                " ? Help  o Settings  1–9 Depth  S System  H Tree  Tab Views  m Measure  q Quit",
                Style::default().fg(ACCENT),
            ),
        ]),
        chunks[4],
    );
    if app.help {
        help(f);
    }
    if app.settings {
        settings(f, app);
    }
    if let Some((ids, signal, label)) = &app.confirmation {
        let name = if *signal == libc::SIGKILL {
            "SIGKILL (force kill)"
        } else {
            "SIGTERM (request exit)"
        };
        let page = area.height.min(23).saturating_sub(10).max(1) as usize;
        let offset = app.confirmation_offset.min(ids.len().saturating_sub(page));
        let mut lines = vec![
            Line::raw(format!(
                "{name} → {}",
                label.chars().take(40).collect::<String>()
            )),
            Line::raw(format!(
                "{} process instances; PID 1 and xtop excluded.",
                ids.len()
            )),
            Line::raw(format!(
                "Targets {}–{}/{} · ↑↓ scroll (list frozen)",
                offset + 1,
                (offset + page).min(ids.len()),
                ids.len()
            )),
        ];
        for id in ids.iter().skip(offset).take(page) {
            let name = app
                .snapshot
                .as_ref()
                .and_then(|s| s.processes.get(&id.pid))
                .filter(|p| p.id == *id)
                .map(|p| p.name.as_str())
                .unwrap_or("exited");
            lines.push(Line::raw(format!(
                "  {}  {}  start={}",
                id.pid, name, id.start
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "y: send signal     n / Esc: cancel",
            Style::default().fg(Color::Yellow),
        ));
        popup(f, "Confirm signal", Text::from(lines), 80, 23);
    }
    table
}

fn summary(f: &mut Frame, area: Rect, app: &App) {
    let Some(s) = &app.snapshot else {
        f.render_widget(Paragraph::new(" Reading /proc…"), area);
        return;
    };
    let m = &s.system;
    let cache = m.cache.min(m.total.saturating_sub(m.free));
    let used = m.total.saturating_sub(m.free + cache);
    let width = area.width.saturating_sub(7) as usize;
    let used_len = (used as f64 / m.total.max(1) as f64 * width as f64) as usize;
    let cache_len = (cache as f64 / m.total.max(1) as f64 * width as f64) as usize;
    let free_len = width.saturating_sub(used_len + cache_len);
    let lines = vec![
        Line::from(vec![
            Span::raw(" RAM  "),
            Span::styled("━".repeat(used_len), Style::default().fg(Color::Magenta)),
            Span::styled("━".repeat(cache_len), Style::default().fg(Color::Yellow)),
            Span::styled("━".repeat(free_len), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" used {}  ", bytes(used)),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(
                format!("cache {}  ", bytes(cache)),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("free {}  ", bytes(m.free)),
                Style::default().fg(Color::Green),
            ),
            Span::raw(format!(
                "available {} / {}",
                bytes(m.available),
                bytes(m.total)
            )),
        ]),
        Line::raw(format!(
            " CPU {:5.1}% total   Swap {} / {}   PSS every {:.0}s (age {:.1}s)",
            m.cpu,
            bytes(m.swap_used),
            bytes(m.swap_total),
            app.config.memory_refresh_ms as f64 / 1000.0,
            s.memory_age.as_secs_f64()
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn details(f: &mut Frame, area: Rect, app: &App) {
    let Some(row) = app.current() else {
        return;
    };
    let Some(s) = &app.snapshot else {
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Details · live totals/history; CPU in cores ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(inner);
    let mut lines = vec![];
    match &row.key {
        Key::Process(id) => {
            if let Some(p) = s.processes.get(&id.pid) {
                let (group, reason) = classify(p, s, &app.config);
                lines.push(Line::raw(format!(
                    "{}{}  PID {} · parent {} · UID {} · state {}",
                    p.name,
                    if row.folded.is_empty() {
                        ""
                    } else {
                        " wrapper"
                    },
                    id.pid,
                    p.ppid,
                    p.uid,
                    p.state
                )));
                let child = row.folded.last().and_then(|id| s.processes.get(&id.pid));
                if let Some(child) = child {
                    lines.push(Line::raw(format!(
                        "Child: {} PID {} · t/x: child; K: chain + tree",
                        crate::model::process_name(child),
                        child.id.pid
                    )));
                }
                let p = child.unwrap_or(p);
                lines.push(Line::raw(format!(
                    "Age: {}{}",
                    age_text(p.age),
                    if p.session_title.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", p.session_title)
                    }
                )));
                lines.push(Line::raw(format!("Command: {}", display_home(&p.command))));
                lines.push(Line::raw(format!(
                    "Directory: {}",
                    if p.cwd.is_empty() {
                        "unavailable".into()
                    } else {
                        display_home(&p.cwd)
                    }
                )));
                lines.push(Line::raw(format!(
                    "Group: {} ({reason})",
                    display_home(&group)
                )));
                lines.push(Line::raw(format!("Cgroup: {}", p.cgroup)));
                lines.push(Line::raw(format!(
                    "Self RSS: {} · self PSS: {} · terminal: {}",
                    bytes(p.rss),
                    p.memory
                        .as_ref()
                        .map(|m| bytes(m.pss))
                        .unwrap_or_else(|| "unavailable".into()),
                    if p.tty.is_empty() {
                        "none / unknown"
                    } else {
                        &p.tty
                    }
                )));
            }
        }
        Key::Group(name) => {
            lines.push(Line::raw(name.replace('\x1f', " / ")));
            lines.push(Line::raw(format!(
                "{} processes · PSS readable for {}",
                row.metrics.count, row.metrics.covered
            )));
            lines.push(Line::raw(format!(
                "PSS {} + SwapPss {} = {}",
                row.metrics.pss_text(),
                row.metrics.swap_text(),
                row.metrics.total_text()
            )));
            if app.view == View::Cgroups {
                if let Some(cg) = s.cgroups.get(name) {
                    lines.push(Line::raw(format!(
                        "Cgroup charge: RAM {} · swap {}",
                        cg.memory.map(bytes).unwrap_or_else(|| "—".into()),
                        cg.swap.map(bytes).unwrap_or_else(|| "—".into())
                    )));
                }
                lines.push(Line::raw(
                    "Cgroup charge includes hidden processes and cache/kernel memory.",
                ));
            } else {
                lines.push(Line::raw(
                    "Display groups do not move processes between cgroups.",
                ));
            }
            lines.push(Line::raw(
                "Enter expands · Shift+K previews termination targets.",
            ));
        }
    }
    f.render_widget(Paragraph::new(lines), parts[0]);
    let charts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(parts[1]);
    let h = app.history.get(&row.key);
    let cpu: Vec<_> = h
        .into_iter()
        .flatten()
        .map(|p| (p.metrics.cpu * 10.0) as u64)
        .collect();
    let ram: Vec<_> = h
        .into_iter()
        .flatten()
        .map(|p| p.metrics.pss / 1024)
        .collect();
    let swap: Vec<_> = h
        .into_iter()
        .flatten()
        .map(|p| p.metrics.swap / 1024)
        .collect();
    let elapsed = h
        .and_then(|h| h.front().zip(h.back()))
        .map(|(a, b)| b.at.duration_since(a.at).as_secs())
        .unwrap_or(0);
    // Fit the whole retained window to the panel; bucket maxima preserve spikes.
    for (rect, data, name, color, value) in [
        (
            charts[0],
            cpu,
            "CPU cores",
            Color::Cyan,
            row.metrics.cpu_text(),
        ),
        (
            charts[1],
            ram,
            "PSS",
            Color::Magenta,
            row.metrics.pss_text(),
        ),
        (
            charts[2],
            swap,
            "SwapPss",
            Color::Yellow,
            row.metrics.swap_text(),
        ),
    ] {
        let data = buckets(&data, rect.width as usize);
        f.render_widget(
            Sparkline::default()
                .block(Block::default().title(format!("{name} {value} · {elapsed}s")))
                .data(&data)
                .style(Style::default().fg(color)),
            rect,
        );
    }
}

fn buckets(values: &[u64], width: usize) -> Vec<u64> {
    if values.len() <= width || width == 0 {
        return values.to_vec();
    }
    (0..width)
        .map(|i| {
            values[i * values.len() / width..(i + 1) * values.len() / width]
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
        })
        .collect()
}

fn popup(f: &mut Frame, title: &str, text: Text, width: u16, height: u16) {
    let size = f.area();
    let width = width.min(size.width.saturating_sub(2));
    let height = height.min(size.height.saturating_sub(2));
    let rect = Rect::new(
        size.x + (size.width - width) / 2,
        size.y + (size.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(ACCENT)),
        ),
        rect,
    );
}

fn help(f: &mut Frame) {
    popup(
        f,
        " Help · Esc closes ",
        Text::raw(
            "NAVIGATION\n↑↓ / j k   Select        PgUp/PgDn   Page\nEnter/Space Expand/collapse node     ← Parent/collapse\n→ Expand/child   e Expand visible level   c Collapse all\nTab Workloads → Process tree → Cgroups\n1–9 Show that many levels (1 = roots only)\nS Show/hide system   H Hierarchical/flat workloads\n/ Search name, command, directory, terminal, PID, cgroup\nEsc Clear search    s Sort name/PSS/CPU    p Pause\nm Measurement: live → 1m avg → 5m avg → 1m peak → 5m peak\nd Details/history   o Settings    q Quit\nMouse: wheel navigates; click selects; click again expands.\n\nPROCESS CONTROL (always previews before sending)\nt SIGTERM selected process     x SIGKILL selected process\nK SIGTERM selected group/subtree (Shift+K)\nSignals bind to process identity via pidfd; permissions apply.\n\nACCOUNTING\nMemory columns: decimal MB (1 MB = 1,000,000 bytes).\nCyan ≥100 MB · amber ≥1,000 MB · red ≥4,000 MB.\nPSS divides shared RAM between its users; SwapPss does\nthe same for swap (shared-memory swap has kernel limits).\n* = partial total; — = unavailable. No RSS substitution.\nTree totals include visible descendants; search keeps totals.\nHistory is in-memory, scoped to current view; gaps/unknown\nsamples are not proof of zero usage. CPU column: logical cores.\nRoot can improve /proc coverage; ordinary use needs no root.\n\nGROUPING\nConfig rules first; application identity unifies scopes.\nSystem means Linux/KDE infrastructure, regardless of UID.\nDetached historical ancestry is not recorded yet.",
        ),
        78,
        32,
    );
}

fn settings(f: &mut Frame, app: &App) {
    let choices = [
        ("PID column", app.show_pid),
        ("CPU column", app.show_cpu),
        ("RAM column", app.show_pss),
        ("SWAP column", app.show_swap),
        ("Footprint column", app.show_total),
        ("Details and history", app.details),
        ("Mouse interaction", app.config.mouse),
        ("Show system processes (S)", app.show_system),
        ("Hierarchical workloads (H)", app.workload_tree),
        ("Process age column", app.show_age),
    ];
    let mut options: Vec<_> = choices
        .iter()
        .map(|(name, on)| format!("[{}] {name}", if *on { "x" } else { " " }))
        .collect();
    options.push(format!(
        "Measurement (m): {} (Enter cycles)",
        app.measurement.label()
    ));
    let visible = (f.area().height.saturating_sub(10) as usize).clamp(1, options.len());
    let start = app.settings_cursor.saturating_sub(visible - 1);
    let mut lines = vec![
        Line::raw("↑↓ select · Enter/Space toggle · Esc close"),
        Line::raw(""),
    ];
    for (i, text) in options.iter().enumerate().skip(start).take(visible) {
        lines.push(Line::styled(
            format!(
                "{} {text}",
                if i == app.settings_cursor { "›" } else { " " }
            ),
            Style::default().fg(if i == app.settings_cursor {
                ACCENT
            } else {
                Color::White
            }),
        ));
    }
    lines.push(Line::raw(format!(
        "Options {}–{} / {}",
        start + 1,
        (start + visible).min(options.len()),
        options.len()
    )));
    lines.push(Line::raw(
        "History: * = partial window/coverage; — = no samples.",
    ));
    lines.push(Line::raw(
        "Settings save automatically; widen for more columns.",
    ));
    lines.push(Line::raw(display_home(
        &app.config_path.display().to_string(),
    )));
    popup(f, " Settings ", Text::from(lines), 76, (visible + 8) as u16);
}

// Compare formatted values, including coverage markers, only within this viewport.
fn ditto(value: String, previous: &mut Option<String>) -> String {
    let zero = value
        .trim_end_matches('*')
        .trim()
        .trim_end_matches(" MB")
        .parse::<f64>()
        .is_ok_and(|n| n == 0.0);
    let repeated = value != "—" && !zero && previous.as_ref() == Some(&value);
    let display = if repeated {
        if value.ends_with('*') { "\"*" } else { "\"" }.to_string()
    } else {
        value.clone()
    };
    *previous = Some(value);
    display
}

fn cpu_cell(text: String, metrics: &crate::model::Metrics) -> Cell<'static> {
    let color =
        if metrics.cpu_covered == 0 || metrics.cpu_text().trim_end_matches('*').trim() == "0" {
            CWD_COLOR
        } else if metrics.cpu >= 400.0 {
            Color::Rgb(255, 115, 115)
        } else if metrics.cpu >= 100.0 {
            Color::Rgb(245, 193, 85)
        } else if metrics.cpu >= 10.0 {
            Color::Rgb(100, 205, 220)
        } else {
            Color::Rgb(200, 210, 220)
        };
    let text = if text.ends_with('*') {
        text
    } else {
        format!("{text} ")
    };
    Cell::from(Line::from(text).right_aligned()).style(Style::default().fg(color))
}

// Fixed decimal MB and a reserved marker position keep decimal points aligned.
fn memory_cell(text: String, bytes: u64, covered: usize) -> Cell<'static> {
    let text = text.replace(" MB", "");
    let text = if text.ends_with('*') {
        text
    } else {
        format!("{text} ")
    };
    let color = if covered == 0 || bytes == 0 {
        CWD_COLOR
    } else if bytes >= 4_000_000_000 {
        Color::Rgb(255, 115, 115)
    } else if bytes >= 1_000_000_000 {
        Color::Rgb(245, 193, 85)
    } else if bytes >= 100_000_000 {
        Color::Rgb(100, 205, 220)
    } else {
        Color::Rgb(200, 210, 220)
    };
    Cell::from(Line::from(text).right_aligned()).style(Style::default().fg(color))
}

fn age_text(age: Option<std::time::Duration>) -> String {
    let Some(age) = age else {
        return "—".into();
    };
    let s = age.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{}s", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h{}m", s / 3600, s % 3600 / 60)
    } else {
        format!("{}d{}h", s / 86400, s % 86400 / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_cells_keep_first_value_and_coverage_information() {
        let mut previous = None;
        for (value, expected) in [
            ("12.3 MB", "12.3 MB"),
            ("12.3 MB", "\""),
            ("12.3 MB", "\""),
            ("12.3 MB*", "12.3 MB*"),
            ("12.3 MB*", "\"*"),
            ("—", "—"),
            ("—", "—"),
            ("0.0 MB", "0.0 MB"),
            ("0.0 MB", "0.0 MB"),
        ] {
            assert_eq!(ditto(value.into(), &mut previous), expected);
        }
        let mut new_viewport = None;
        assert_eq!(ditto("0.0 MB".into(), &mut new_viewport), "0.0 MB");
    }

    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn populated_layout_keeps_process_names_and_columns_visible() {
        use crate::procfs::{Identity, Memory, Process, System};
        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
        let mut app = App::new(
            Default::default(),
            "/home/demo/.config/xtop/config.toml".into(),
        );
        let mut processes = std::collections::HashMap::new();
        for (i, name) in ["zsh", "codex", "cargo", "rust-analyzer", "node"]
            .iter()
            .enumerate()
        {
            let pid = 100 + i as u32;
            processes.insert(
                pid,
                Process {
                    id: Identity {
                        pid,
                        start: pid as u64,
                    },
                    ppid: if i == 0 { 1 } else { 100 },
                    name: name.to_string(),
                    command: format!("{name} --demo"),
                    exe: format!("/usr/bin/{name}"),
                    cwd: "/home/demo/hobby/xtop".into(),
                    cgroup: "/user.slice/app-terminal.scope".into(),
                    tty: "/dev/pts/2".into(),
                    tty_nr: 34818,
                    age: Some(std::time::Duration::from_secs(3720)),
                    session_title: "tmux 0:1.0 · Agent task".into(),
                    interactive_shell: true,
                    uid: 1000,
                    state: 'S',
                    ticks: 0,
                    rss: 500 * 1024 * 1024,
                    cpu: Some(i as f64 * 12.5),
                    memory: Some(Memory {
                        pss: (i + 1) as u64 * 100 * 1024 * 1024,
                        swap_pss: 10 * 1024 * 1024,
                    }),
                },
            );
        }
        app.update(Snapshot {
            processes,
            system: System {
                total: 16 << 30,
                free: 4 << 30,
                available: 7 << 30,
                cache: 3 << 30,
                swap_total: 8 << 30,
                swap_used: 1 << 30,
                cpu: 12.5,
            },
            cgroups: Default::default(),
            at: std::time::Instant::now(),
            memory_age: Duration::from_secs(2),
        });
        app.set_depth(4);
        app.selected = Some(Key::Process(crate::procfs::Identity {
            pid: 100,
            start: 100,
        }));
        terminal
            .draw(|f| {
                draw(f, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..32)
            .map(|y| {
                (0..120)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("codex  /home/demo/hobby/xtop"));
        let (y, line) = rendered
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("codex  /home/demo/hobby/xtop"))
            .unwrap();
        let x = line[..line.find("codex  ").unwrap()].chars().count() as u16;
        assert_eq!(buffer[(x + 7, y as u16)].fg, CWD_COLOR);
        assert_ne!(buffer[(x, y as u16)].fg, buffer[(x + 7, y as u16)].fg);
        assert!(rendered.contains("AGE"));
        assert!(rendered.contains("1h2m"));
        assert!(rendered.contains("RAM"));
        assert!(rendered.contains("SWAP"));
        assert!(rendered.contains("Agent task"));
        assert!(rendered.contains("Settings"));
        std::fs::write("target/ui-preview.txt", rendered).unwrap();
        let mut snapshot = app.snapshot.clone().unwrap();
        snapshot.processes.retain(|pid, _| [100, 101].contains(pid));
        app.update(snapshot);
        terminal
            .draw(|f| {
                draw(f, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let folded = (0..32)
            .map(|y| {
                (0..120)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(folded.contains("zsh: codex  /home/demo/hobby/xtop"));
        assert!(folded.contains("Child: codex PID 101"));
        std::fs::write("target/ui-preview-folded.txt", folded).unwrap();
        let mut snapshot = app.snapshot.clone().unwrap();
        let mut leaf = snapshot.processes[&101].clone();
        leaf.id.pid = 102;
        leaf.id.start = 102;
        leaf.ppid = 101;
        let node = snapshot.processes.get_mut(&101).unwrap();
        node.name = "node".into();
        node.exe = "/usr/bin/node".into();
        node.command = "node".into();
        snapshot.processes.insert(102, leaf);
        app.update(snapshot);
        terminal
            .draw(|f| {
                draw(f, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let chain = (0..32)
            .map(|y| {
                (0..120)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(chain.contains("zsh: node: codex  /home/demo/hobby/xtop"));
        assert!(chain.contains("Child: codex PID 102"));
        std::fs::write("target/ui-preview-chain.txt", chain).unwrap();
        // Exercise the shared measurement mode with a complete minute.
        app.history.clear();
        app.config.refresh_ms = 60_000;
        let mut sample = app.snapshot.clone().unwrap();
        app.update(sample.clone());
        sample.at += Duration::from_secs(60);
        app.update(sample);
        app.settings_cursor = 10;
        toggle_setting(&mut app);
        assert_eq!(app.measurement, crate::history::Measurement::Average1m);
        app.show_total = true;
        let mut wide = Terminal::new(TestBackend::new(240, 40)).unwrap();
        wide.draw(|f| {
            draw(f, &mut app);
        })
        .unwrap();
        let buffer = wide.backend().buffer();
        let history = (0..40)
            .map(|y| {
                (0..240)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(history.contains("m: 1 min average"));
        for name in ["CPU", "RAM", "SWAP", "Footprint"] {
            assert!(history.contains(name));
        }
        assert!(!history.contains("avg1m"));
        std::fs::write("target/ui-preview-history.txt", history).unwrap();
        toggle_setting(&mut app);
        assert_eq!(app.measurement, crate::history::Measurement::Average5m);
        app.settings = true;
        let mut narrow = Terminal::new(TestBackend::new(80, 24)).unwrap();
        narrow
            .draw(|f| {
                draw(f, &mut app);
            })
            .unwrap();
        let buffer = narrow.backend().buffer();
        let settings = (0..24)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(settings.contains("Measurement (m): 5 min average"));
    }
    #[test]
    fn renders_small_and_large_terminals_without_panics() {
        for (w, h) in [(40, 10), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut app = App::new(Default::default(), Default::default());
            terminal
                .draw(|f| {
                    draw(f, &mut app);
                })
                .unwrap();
            app.help = true;
            terminal
                .draw(|f| {
                    draw(f, &mut app);
                })
                .unwrap();
        }
    }
}
