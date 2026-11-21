use super::theme;
use super::{
    ActiveTab, App, DownloadsPane, ExportTargetForm, IndexerForm, Modal, ProviderForm,
    SettingsSection, TextPrompt,
};
use crate::config::mask_secret;
use crate::engine::{format_bytes, format_eta, format_speed, JobStatus, JobSummary, LogLevel};
use crate::newznab::CATEGORIES;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    /// Mobile portrait / narrow terminal (< 72 cols)
    Compact,
    /// Landscape phone / tmux split / small terminal (72..100 cols)
    Medium,
    /// Full desktop terminal (>= 100 cols)
    Wide,
}

fn layout_mode(area: Rect) -> LayoutMode {
    if area.width < 72 {
        LayoutMode::Compact
    } else if area.width < 100 {
        LayoutMode::Medium
    } else {
        LayoutMode::Wide
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let mode = layout_mode(area);
    let very_short = area.height < 16;

    let header_height = if mode == LayoutMode::Compact { 2 } else { 3 };
    let footer_height = if very_short { 1 } else { 2 };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(5),
            Constraint::Length(footer_height),
        ])
        .split(area);

    draw_header(frame, app, outer[0], mode);

    match app.active_tab {
        ActiveTab::Search => draw_search(frame, app, outer[1], mode),
        ActiveTab::Downloads => draw_downloads(frame, app, outer[1], mode),
        ActiveTab::Settings => draw_settings(frame, app, outer[1], mode),
        ActiveTab::Logs => draw_logs(frame, app, outer[1], mode),
        ActiveTab::Export => draw_export(frame, app, outer[1], mode),
    }

    draw_footer(frame, app, outer[2], mode, very_short);

    if let Some(ref modal) = app.modal {
        match modal {
            Modal::Help => draw_help_modal(frame, app.modal_scroll, area),
            Modal::Provider(form) => draw_provider_modal(frame, form, area),
            Modal::Indexer(form) => draw_indexer_modal(frame, form, area),
            Modal::ExportTarget(form) => draw_export_target_modal(frame, form, area),
            Modal::TextPrompt(prompt) => draw_text_prompt_modal(frame, prompt, area),
            Modal::JobDetails(job) => draw_job_details_modal(frame, job, app.modal_scroll, area),
        }
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    if mode == LayoutMode::Compact {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let dl_label = if app.snapshot.queue.is_empty() {
            "2:DL".to_string()
        } else {
            format!("2:DL({})", app.snapshot.queue.len())
        };
        let labels = [
            "1:Srch".to_string(),
            dl_label,
            "3:Set".to_string(),
            "4:Log".to_string(),
            "5:Exp".to_string(),
        ];

        let mut spans = Vec::new();
        for (idx, lbl) in labels.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::raw(" "));
            }
            if idx == app.active_tab.index() {
                spans.push(Span::styled(
                    format!(" {} ", lbl),
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(lbl.clone(), theme::subtext()));
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

        let speed = format_speed(app.snapshot.current_speed_bps);
        let state_span = if app.snapshot.global_paused {
            Span::styled(" PAUSED ", Style::default().fg(Color::Black).bg(theme::WARNING))
        } else {
            Span::styled(" LIVE ", Style::default().fg(Color::Black).bg(theme::SUCCESS))
        };
        let mut status_spans = vec![
            state_span,
            Span::raw(" "),
            Span::styled(
                format!("↓{}", speed),
                Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " [{}/{}]",
                    app.snapshot.active_connections, app.snapshot.max_connections
                ),
                theme::subtext(),
            ),
        ];
        if let Some(free) = app.snapshot.free_disk_bytes {
            status_spans.push(Span::styled(
                format!(" {} free", format_bytes(free)),
                theme::muted(),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(status_spans)), rows[1]);
        return;
    }

    let status_width = if mode == LayoutMode::Medium { 30 } else { 38 };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(status_width)])
        .split(area);

    let tab_titles = if mode == LayoutMode::Medium {
        let dl = if app.snapshot.queue.is_empty() {
            "2:DL".to_string()
        } else {
            format!("2:DL({})", app.snapshot.queue.len())
        };
        vec![
            Line::from(" 1:Search "),
            Line::from(format!(" {} ", dl)),
            Line::from(" 3:Set "),
            Line::from(" 4:Log "),
            Line::from(" 5:Exp "),
        ]
    } else {
        let queue_badge = if app.snapshot.queue.is_empty() {
            "2:Downloads".to_string()
        } else {
            format!("2:Downloads ({})", app.snapshot.queue.len())
        };
        vec![
            Line::from(" 1:Search "),
            Line::from(format!(" {} ", queue_badge)),
            Line::from(" 3:Settings "),
            Line::from(" 4:Logs "),
            Line::from(" 5:Export "),
        ]
    };

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(
                    concat!(" nzbtui v", env!("CARGO_PKG_VERSION"), " "),
                    theme::title(),
                )),
        )
        .select(app.active_tab.index())
        .style(theme::subtext())
        .highlight_style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );

    frame.render_widget(tabs, chunks[0]);

    let speed = format_speed(app.snapshot.current_speed_bps);
    let pause_span = if app.snapshot.global_paused {
        Span::styled(" PAUSED ", Style::default().fg(Color::Black).bg(theme::WARNING))
    } else {
        Span::styled(" LIVE ", Style::default().fg(Color::Black).bg(theme::SUCCESS))
    };

    let status_line = Line::from(vec![
        pause_span,
        Span::raw(" "),
        Span::styled(
            format!("↓ {}", speed),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " [{}/{}]",
                app.snapshot.active_connections, app.snapshot.max_connections
            ),
            theme::subtext(),
        ),
    ]);

    let status_title = match (
        app.snapshot.speed_limit_kbps > 0,
        app.snapshot.free_disk_bytes,
    ) {
        (true, Some(free)) if mode == LayoutMode::Wide => format!(
            " Status (cap {} KB/s | {} free) ",
            app.snapshot.speed_limit_kbps,
            format_bytes(free)
        ),
        (true, _) => format!(" Status (cap {}K) ", app.snapshot.speed_limit_kbps),
        (false, Some(free)) => format!(" Status ({} free) ", format_bytes(free)),
        (false, None) => " Status ".to_string(),
    };

    let status_widget = Paragraph::new(status_line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(status_title, theme::subtext())),
    );
    frame.render_widget(status_widget, chunks[1]);
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let cat_label = CATEGORIES[app.search_category_idx].label;
    let season_label = if app.search_season.trim().is_empty() {
        "All".to_string()
    } else if app.search_season.trim() == "00" {
        if mode == LayoutMode::Compact {
            "00".to_string()
        } else {
            "00 (Packs)".to_string()
        }
    } else {
        format!("S{}", app.search_season.trim())
    };
    let ep_label = if app.search_episode.trim().is_empty() {
        "All".to_string()
    } else {
        format!("E{}", app.search_episode.trim())
    };

    let results_area = match mode {
        LayoutMode::Compact => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Min(4),
                ])
                .split(area);

            draw_search_input_box(frame, app, chunks[0], true);

            let filter_line = Line::from(vec![
                Span::styled(" c:", theme::muted()),
                Span::styled(cat_label, Style::default().fg(theme::TEXT)),
                Span::styled(" S:", theme::muted()),
                Span::styled(
                    season_label,
                    if app.search_season.trim().is_empty() {
                        theme::subtext()
                    } else {
                        theme::title()
                    },
                ),
                Span::styled(" E:", theme::muted()),
                Span::styled(
                    ep_label,
                    if app.search_episode.trim().is_empty() {
                        theme::subtext()
                    } else {
                        theme::title()
                    },
                ),
                Span::styled(" i:", theme::muted()),
                Span::styled(app.current_indexer_filter_label(), theme::subtext()),
                Span::styled(" o:", theme::muted()),
                Span::styled(app.search_sort.label(), theme::subtext()),
            ]);
            frame.render_widget(Paragraph::new(filter_line), chunks[1]);
            chunks[2]
        }
        LayoutMode::Medium => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Min(4),
                ])
                .split(area);

            let bar_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(24),
                    Constraint::Length(13),
                    Constraint::Length(11),
                ])
                .split(chunks[0]);

            draw_search_input_box(frame, app, bar_chunks[0], false);

            let cat_box = Paragraph::new(cat_label)
                .style(Style::default().fg(theme::TEXT))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Cat [c] ", theme::subtext())),
                );
            frame.render_widget(cat_box, bar_chunks[1]);

            let sort_box = Paragraph::new(app.search_sort.label())
                .style(Style::default().fg(theme::TEXT))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Sort [o] ", theme::subtext())),
                );
            frame.render_widget(sort_box, bar_chunks[2]);

            let filter_line = Line::from(vec![
                Span::styled(" Season [S]: ", theme::muted()),
                Span::styled(
                    season_label,
                    if app.search_season.trim().is_empty() {
                        theme::subtext()
                    } else {
                        theme::title()
                    },
                ),
                Span::styled("  |  Ep [E]: ", theme::muted()),
                Span::styled(
                    ep_label,
                    if app.search_episode.trim().is_empty() {
                        theme::subtext()
                    } else {
                        theme::title()
                    },
                ),
                Span::styled("  |  Indexer [i]: ", theme::muted()),
                Span::styled(app.current_indexer_filter_label(), theme::subtext()),
            ]);
            frame.render_widget(Paragraph::new(filter_line), chunks[1]);
            chunks[2]
        }
        LayoutMode::Wide => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(5)])
                .split(area);

            let bar_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(20),
                    Constraint::Length(14),
                    Constraint::Length(14),
                    Constraint::Length(12),
                    Constraint::Length(16),
                    Constraint::Length(11),
                ])
                .split(chunks[0]);

            draw_search_input_box(frame, app, bar_chunks[0], false);

            let cat_box = Paragraph::new(cat_label)
                .style(Style::default().fg(theme::TEXT))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Cat [c] ", theme::subtext())),
                );
            frame.render_widget(cat_box, bar_chunks[1]);

            let season_style = if app.search_season.trim().is_empty() {
                Style::default().fg(theme::TEXT)
            } else {
                theme::title()
            };
            let season_box = Paragraph::new(season_label)
                .style(season_style)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Season [S] ", theme::subtext())),
                );
            frame.render_widget(season_box, bar_chunks[2]);

            let ep_style = if app.search_episode.trim().is_empty() {
                Style::default().fg(theme::TEXT)
            } else {
                theme::title()
            };
            let ep_box = Paragraph::new(ep_label)
                .style(ep_style)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Ep [E] ", theme::subtext())),
                );
            frame.render_widget(ep_box, bar_chunks[3]);

            let idx_box = Paragraph::new(app.current_indexer_filter_label())
                .style(Style::default().fg(theme::TEXT))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Indexer [i] ", theme::subtext())),
                );
            frame.render_widget(idx_box, bar_chunks[4]);

            let sort_box = Paragraph::new(app.search_sort.label())
                .style(Style::default().fg(theme::TEXT))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(Span::styled(" Sort [o] ", theme::subtext())),
                );
            frame.render_widget(sort_box, bar_chunks[5]);

            chunks[1]
        }
    };

    // Results table
    if app.search_results.is_empty() {
        let msg = if app.search_loading {
            "Querying Newznab indexers…".to_string()
        } else if app.config.enabled_indexers().is_empty() {
            "No indexers configured.\n\nPress [3] Settings -> [l] Indexers -> [a] Add.\nOr press [I] to import a local .nzb file.".to_string()
        } else {
            "No results yet. Press [/] to search or [r] for latest.".to_string()
        };
        let empty = Paragraph::new(msg)
            .style(theme::subtext())
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .title(Span::styled(" Results ", theme::title())),
            );
        frame.render_widget(empty, results_area);
        return;
    }

    let tiny = results_area.width < 52;
    let title_str = match mode {
        LayoutMode::Compact => {
            if app.search_marked.is_empty() {
                format!(" Results ({}) p:{} ", app.search_results.len(), app.search_offset)
            } else {
                format!(
                    " Results ({}, {}*) ",
                    app.search_results.len(),
                    app.search_marked.len()
                )
            }
        }
        LayoutMode::Medium => format!(
            " Results ({}, off {}, {} marked) — [Enter/d] Queue  [[/]] Page ",
            app.search_results.len(),
            app.search_offset,
            app.search_marked.len()
        ),
        LayoutMode::Wide => format!(
            " Results ({}, offset {}, {} marked) — [Space] Mark  [Enter/d] Queue  [[/]] Page ",
            app.search_results.len(),
            app.search_offset,
            app.search_marked.len()
        ),
    };

    let (header, constraints): (Row, Vec<Constraint>) = match (mode, tiny) {
        (LayoutMode::Compact, true) => (
            Row::new(vec!["Title", "Size"]).style(theme::table_header()),
            vec![Constraint::Min(18), Constraint::Length(9)],
        ),
        (LayoutMode::Compact, false) => (
            Row::new(vec!["Title", "Size", "Age"]).style(theme::table_header()),
            vec![
                Constraint::Min(22),
                Constraint::Length(9),
                Constraint::Length(5),
            ],
        ),
        (LayoutMode::Medium, _) => (
            Row::new(vec!["Title", "Size", "Age", "Grabs"]).style(theme::table_header()),
            vec![
                Constraint::Min(30),
                Constraint::Length(10),
                Constraint::Length(6),
                Constraint::Length(6),
            ],
        ),
        (LayoutMode::Wide, _) => (
            Row::new(vec!["Title", "Size", "Age", "Grabs", "Indexer", "Category"])
                .style(theme::table_header()),
            vec![
                Constraint::Min(36),
                Constraint::Length(11),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(14),
                Constraint::Length(18),
            ],
        ),
    };

    let rows: Vec<Row> = app
        .search_results
        .iter()
        .map(|item| {
            let queued_prefix = if app.queued_guids.contains(&item.guid) {
                "[✓] "
            } else if app.search_marked.contains(&item.guid) {
                "[*] "
            } else if item.password_protected {
                "[🔒] "
            } else {
                ""
            };
            let title_text = format!("{}{}", queued_prefix, item.title);
            let title_style = if app.queued_guids.contains(&item.guid) {
                theme::success()
            } else if app.search_marked.contains(&item.guid) {
                theme::title()
            } else {
                Style::default().fg(theme::TEXT)
            };

            let mut cells = vec![
                Cell::from(title_text).style(title_style),
                Cell::from(format_bytes(item.size)).style(Style::default().fg(theme::ACCENT)),
            ];
            if !(mode == LayoutMode::Compact && tiny) {
                cells.push(Cell::from(item.age_display()).style(theme::subtext()));
            }
            if matches!(mode, LayoutMode::Medium | LayoutMode::Wide) {
                cells.push(Cell::from(item.grabs.to_string()).style(theme::subtext()));
            }
            if mode == LayoutMode::Wide {
                cells.push(Cell::from(item.indexer_name.clone()).style(theme::subtext()));
                cells.push(Cell::from(item.category.clone()).style(theme::muted()));
            }
            Row::new(cells)
        })
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if !app.search_input_focused {
                    theme::border_focused()
                } else {
                    theme::border()
                })
                .title(Span::styled(title_str, theme::title())),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.search_selected));
    frame.render_stateful_widget(table, results_area, &mut state);
}

fn draw_search_input_box(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    let inner_w = area.width.saturating_sub(3) as usize;
    let query_display = if app.search_input_focused {
        format!("{}▌", tail_fit(&app.search_query, inner_w))
    } else if app.search_query.is_empty() {
        if compact {
            "Press [/] to search…".to_string()
        } else {
            "Press [/] to search Newznab indexers (empty = latest)…".to_string()
        }
    } else {
        app.search_query.clone()
    };

    let query_style = if !app.search_input_focused && app.search_query.is_empty() {
        theme::muted()
    } else {
        Style::default().fg(theme::TEXT)
    };

    let search_input = Paragraph::new(query_display)
        .style(query_style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if app.search_input_focused {
                    theme::border_focused()
                } else {
                    theme::border()
                })
                .title(Span::styled(
                    if app.search_loading {
                        " Search (querying…) "
                    } else {
                        " Search [/] "
                    },
                    if app.search_input_focused {
                        theme::title()
                    } else {
                        theme::subtext()
                    },
                )),
        );
    frame.render_widget(search_input, area);
}

fn draw_downloads(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let single_pane = area.height < 18;
    let peak_bps = app
        .snapshot
        .speed_history
        .iter()
        .copied()
        .max()
        .unwrap_or(0);

    if single_pane {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(4)])
            .split(area);

        let summary = Line::from(vec![
            Span::styled(
                format!(" ↓ {} ", format_speed(app.snapshot.current_speed_bps)),
                theme::title(),
            ),
            Span::styled(format!("(peak {}) ", format_speed(peak_bps)), theme::muted()),
            Span::styled(
                format!(
                    "Q:{} H:{} [w]switch",
                    app.snapshot.queue.len(),
                    app.snapshot.history.len()
                ),
                theme::subtext(),
            ),
        ]);
        frame.render_widget(Paragraph::new(summary), chunks[0]);

        match app.downloads_pane {
            DownloadsPane::Queue => draw_queue_table(frame, app, chunks[1], mode, true),
            DownloadsPane::History => draw_history_table(frame, app, chunks[1], mode, true),
        }
        return;
    }

    let spark_height = if area.height < 23 { 3 } else { 5 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(spark_height),
            Constraint::Percentage(55),
            Constraint::Percentage(45),
        ])
        .split(area);

    let spark_data: Vec<u64> = app
        .snapshot
        .speed_history
        .iter()
        .map(|&bps| bps / 1024)
        .collect();

    let spark_title = if mode == LayoutMode::Compact {
        format!(
            " {} | Peak {} ",
            format_speed(app.snapshot.current_speed_bps),
            format_speed(peak_bps)
        )
    } else {
        format!(
            " Throughput (60s) — Now: {} | Peak: {} | Connections: {}/{} ",
            format_speed(app.snapshot.current_speed_bps),
            format_speed(peak_bps),
            app.snapshot.active_connections,
            app.snapshot.max_connections
        )
    };

    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(spark_title, theme::title())),
        )
        .data(&spark_data)
        .style(Style::default().fg(theme::ACCENT));
    frame.render_widget(sparkline, chunks[0]);

    draw_queue_table(frame, app, chunks[1], mode, false);
    draw_history_table(frame, app, chunks[2], mode, false);
}

fn draw_queue_table(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    mode: LayoutMode,
    single_pane: bool,
) {
    let queue_focused = app.downloads_pane == DownloadsPane::Queue;
    let tiny = area.width < 52;

    let (q_header, constraints): (Row, Vec<Constraint>) = match (mode, tiny) {
        (LayoutMode::Compact, true) => (
            Row::new(vec!["St", "Title", "%"]).style(theme::table_header()),
            vec![
                Constraint::Length(6),
                Constraint::Min(14),
                Constraint::Length(5),
            ],
        ),
        (LayoutMode::Compact, false) => (
            Row::new(vec!["State", "Title", "Prog", "ETA"]).style(theme::table_header()),
            vec![
                Constraint::Length(7),
                Constraint::Min(16),
                Constraint::Length(10),
                Constraint::Length(6),
            ],
        ),
        (LayoutMode::Medium, _) => (
            Row::new(vec!["State", "Title", "Progress", "Size", "ETA"])
                .style(theme::table_header()),
            vec![
                Constraint::Length(8),
                Constraint::Min(20),
                Constraint::Length(14),
                Constraint::Length(17),
                Constraint::Length(7),
            ],
        ),
        (LayoutMode::Wide, _) => (
            Row::new(vec!["State", "Title", "Progress", "Size", "ETA", "Detail"])
                .style(theme::table_header()),
            vec![
                Constraint::Length(9),
                Constraint::Min(26),
                Constraint::Length(18),
                Constraint::Length(20),
                Constraint::Length(8),
                Constraint::Length(26),
            ],
        ),
    };

    let q_rows: Vec<Row> = app
        .snapshot
        .queue
        .iter()
        .map(|job| {
            let pct = (job.progress_ratio() * 100.0).round() as u16;
            let bar_w = match mode {
                LayoutMode::Compact => 4,
                LayoutMode::Medium => 8,
                LayoutMode::Wide => 12,
            };
            let prog_str = if mode == LayoutMode::Compact && tiny {
                format!("{:3}%", pct)
            } else {
                format!("{} {:3}%", ascii_progress_bar(job.progress_ratio(), bar_w), pct)
            };

            let mut cells = vec![
                Cell::from(job.status.label()).style(status_style(job.status)),
                Cell::from(job.title.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(prog_str).style(Style::default().fg(theme::ACCENT)),
            ];
            if mode == LayoutMode::Compact && !tiny {
                cells.push(Cell::from(format_eta(job.eta_seconds())).style(theme::subtext()));
            } else if matches!(mode, LayoutMode::Medium | LayoutMode::Wide) {
                cells.push(
                    Cell::from(format!(
                        "{}/{}",
                        format_bytes(job.downloaded_bytes),
                        format_bytes(job.total_bytes)
                    ))
                    .style(theme::subtext()),
                );
                cells.push(Cell::from(format_eta(job.eta_seconds())).style(theme::subtext()));
                if mode == LayoutMode::Wide {
                    cells.push(Cell::from(job.stage_detail.clone()).style(theme::muted()));
                }
            }
            Row::new(cells)
        })
        .collect();

    let title = if single_pane || mode == LayoutMode::Compact {
        format!(
            " Queue ({}) [w->Hist ({})] ",
            app.snapshot.queue.len(),
            app.snapshot.history.len()
        )
    } else if mode == LayoutMode::Medium {
        format!(
            " Queue ({}) — [Space] Pause [x] Cancel [w] Switch ",
            app.snapshot.queue.len()
        )
    } else {
        format!(
            " Active Queue ({}) — [Space] Pause  [x] Cancel  [K/J] Reorder  [w] Switch Pane ",
            app.snapshot.queue.len()
        )
    };

    let queue_table = Table::new(q_rows, constraints)
        .header(q_header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if queue_focused {
                    theme::border_focused()
                } else {
                    theme::border()
                })
                .title(Span::styled(
                    title,
                    if queue_focused {
                        theme::title()
                    } else {
                        theme::subtext()
                    },
                )),
        )
        .row_highlight_style(if queue_focused {
            theme::selected_row()
        } else {
            Style::default()
        })
        .highlight_symbol("▸ ");

    let mut q_state = TableState::default();
    if !app.snapshot.queue.is_empty() {
        q_state.select(Some(app.queue_selected));
    }
    frame.render_stateful_widget(queue_table, area, &mut q_state);
}

fn draw_history_table(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    mode: LayoutMode,
    single_pane: bool,
) {
    let hist_focused = app.downloads_pane == DownloadsPane::History;

    let (h_header, constraints): (Row, Vec<Constraint>) = match mode {
        LayoutMode::Compact => (
            Row::new(vec!["Res", "Title", "Size"]).style(theme::table_header()),
            vec![
                Constraint::Length(6),
                Constraint::Min(16),
                Constraint::Length(9),
            ],
        ),
        LayoutMode::Medium => (
            Row::new(vec!["Result", "Title", "Size", "Info"]).style(theme::table_header()),
            vec![
                Constraint::Length(7),
                Constraint::Min(20),
                Constraint::Length(10),
                Constraint::Percentage(28),
            ],
        ),
        LayoutMode::Wide => (
            Row::new(vec!["Result", "Title", "Size", "Output / Error"])
                .style(theme::table_header()),
            vec![
                Constraint::Length(9),
                Constraint::Min(28),
                Constraint::Length(12),
                Constraint::Min(28),
            ],
        ),
    };

    let h_rows: Vec<Row> = app
        .snapshot
        .history
        .iter()
        .map(|job| {
            let info = if let Some(ref p) = job.output_path {
                p.display().to_string()
            } else if let Some(ref e) = job.error {
                e.clone()
            } else {
                job.stage_detail.clone()
            };
            let mut cells = vec![
                Cell::from(job.status.label()).style(status_style(job.status)),
                Cell::from(job.title.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(format_bytes(job.total_bytes)).style(theme::subtext()),
            ];
            if mode != LayoutMode::Compact {
                cells.push(Cell::from(info).style(if job.status == JobStatus::Failed {
                    theme::danger()
                } else {
                    theme::muted()
                }));
            }
            Row::new(cells)
        })
        .collect();

    let title = if single_pane || mode == LayoutMode::Compact {
        format!(
            " History ({}) [w->Queue ({})] ",
            app.snapshot.history.len(),
            app.snapshot.queue.len()
        )
    } else if mode == LayoutMode::Medium {
        format!(
            " History ({}) — [Enter] Info [r] Retry [x] Del [C] Clear ",
            app.snapshot.history.len()
        )
    } else {
        format!(
            " History ({}) — [Enter] Details  [r] Retry  [o] Open  [x] Remove  [c] Cleanup  [C] Clear All ",
            app.snapshot.history.len()
        )
    };

    let hist_table = Table::new(h_rows, constraints)
        .header(h_header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if hist_focused {
                    theme::border_focused()
                } else {
                    theme::border()
                })
                .title(Span::styled(
                    title,
                    if hist_focused {
                        theme::title()
                    } else {
                        theme::subtext()
                    },
                )),
        )
        .row_highlight_style(if hist_focused {
            theme::selected_row()
        } else {
            Style::default()
        })
        .highlight_symbol("▸ ");

    let mut h_state = TableState::default();
    if !app.snapshot.history.is_empty() {
        h_state.select(Some(app.history_selected));
    }
    frame.render_stateful_widget(hist_table, area, &mut h_state);
}

fn draw_settings(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(area);

    let sub_titles = match mode {
        LayoutMode::Compact => vec![
            Line::from(format!("Prov({})", app.config.providers.len())),
            Line::from(format!("Idx({})", app.config.indexers.len())),
            Line::from("Gen"),
            Line::from(format!("Exp({})", app.config.export_targets.len())),
        ],
        LayoutMode::Medium => vec![
            Line::from(format!(" Providers ({}) ", app.config.providers.len())),
            Line::from(format!(" Indexers ({}) ", app.config.indexers.len())),
            Line::from(" General "),
            Line::from(format!(" Export ({}) ", app.config.export_targets.len())),
        ],
        LayoutMode::Wide => vec![
            Line::from(format!(" Usenet Providers ({}) ", app.config.providers.len())),
            Line::from(format!(" Newznab Indexers ({}) ", app.config.indexers.len())),
            Line::from(" General & Post-Processing "),
            Line::from(format!(
                " Export Targets ({}) ",
                app.config.export_targets.len()
            )),
        ],
    };
    let sub_idx = match app.settings_section {
        SettingsSection::Providers => 0,
        SettingsSection::Indexers => 1,
        SettingsSection::General => 2,
        SettingsSection::Export => 3,
    };

    let section_title = if mode == LayoutMode::Compact {
        " Settings [h/l] ".to_string()
    } else {
        format!(
            " Settings [h/l switch section] — Config: {} ",
            app.config_path.display()
        )
    };

    let sub_tabs = Tabs::new(sub_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(section_title, theme::subtext())),
        )
        .select(sub_idx)
        .style(theme::subtext())
        .highlight_style(theme::title());
    frame.render_widget(sub_tabs, chunks[0]);

    match app.settings_section {
        SettingsSection::Providers => draw_providers_settings(frame, app, chunks[1], mode),
        SettingsSection::Indexers => draw_indexers_settings(frame, app, chunks[1], mode),
        SettingsSection::General => draw_general_settings(frame, app, chunks[1], mode),
        SettingsSection::Export => draw_export_targets_settings(frame, app, chunks[1], mode),
    }
}

fn draw_providers_settings(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let title = match (app.testing_status.as_deref(), mode) {
        (Some(s), _) => format!(" Providers — {} ", s),
        (None, LayoutMode::Compact) => " Providers [a/e/t/d] ".to_string(),
        (None, _) => {
            " Usenet Providers — [a] Add  [e] Edit  [t] Test Connection  [Space] Toggle  [d] Delete "
                .to_string()
        }
    };

    if app.config.providers.is_empty() {
        let p = Paragraph::new(
            "No Usenet providers configured.\n\nPress [a] to add your NNTP server (host, port 563 TLS, username, password, connections, priority 0).",
        )
        .style(theme::subtext())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        );
        frame.render_widget(p, area);
        return;
    }

    let (header, constraints): (Row, Vec<Constraint>) = match mode {
        LayoutMode::Compact => (
            Row::new(vec!["On", "Name", "Host:Port"]).style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Percentage(38),
                Constraint::Percentage(58),
            ],
        ),
        LayoutMode::Medium => (
            Row::new(vec!["On", "Pri", "Name", "Host:Port", "TLS", "Conn"])
                .style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Length(4),
                Constraint::Length(14),
                Constraint::Min(18),
                Constraint::Length(6),
                Constraint::Length(5),
            ],
        ),
        LayoutMode::Wide => (
            Row::new(vec![
                "On",
                "Pri",
                "Name",
                "Host:Port",
                "TLS",
                "Username",
                "Password",
                "Conns",
            ])
            .style(theme::table_header()),
            vec![
                Constraint::Length(4),
                Constraint::Length(5),
                Constraint::Length(18),
                Constraint::Min(24),
                Constraint::Length(7),
                Constraint::Length(18),
                Constraint::Length(12),
                Constraint::Length(7),
            ],
        ),
    };

    let rows: Vec<Row> = app
        .config
        .providers
        .iter()
        .map(|p| {
            let on_cell = Cell::from(if p.enabled { "●" } else { "○" }).style(if p.enabled {
                theme::success()
            } else {
                theme::muted()
            });
            let name_cell = Cell::from(p.name.clone()).style(Style::default().fg(theme::TEXT));
            let host_cell = Cell::from(format!("{}:{}", p.host, p.port)).style(theme::subtext());

            match mode {
                LayoutMode::Compact => Row::new(vec![on_cell, name_cell, host_cell]),
                LayoutMode::Medium => Row::new(vec![
                    on_cell,
                    Cell::from(p.priority.to_string()).style(Style::default().fg(theme::ACCENT)),
                    name_cell,
                    host_cell,
                    Cell::from(if p.tls { "TLS" } else { "No" }).style(if p.tls {
                        theme::success()
                    } else {
                        theme::warning()
                    }),
                    Cell::from(p.connections.to_string()).style(theme::subtext()),
                ]),
                LayoutMode::Wide => Row::new(vec![
                    on_cell,
                    Cell::from(p.priority.to_string()).style(Style::default().fg(theme::ACCENT)),
                    name_cell,
                    host_cell,
                    Cell::from(if p.tls { "TLS" } else { "Plain" }).style(if p.tls {
                        theme::success()
                    } else {
                        theme::warning()
                    }),
                    Cell::from(if p.username.is_empty() {
                        "(anon)".to_string()
                    } else {
                        p.username.clone()
                    })
                    .style(theme::subtext()),
                    Cell::from(mask_secret(&p.password)).style(theme::muted()),
                    Cell::from(p.connections.to_string()).style(theme::subtext()),
                ]),
            }
        })
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.provider_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_indexers_settings(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let title = match (app.testing_status.as_deref(), mode) {
        (Some(s), _) => format!(" Indexers — {} ", s),
        (None, LayoutMode::Compact) => " Indexers [a/e/t/d] ".to_string(),
        (None, _) => {
            " Newznab Indexers — [a] Add  [e] Edit  [t] Test API Key  [Space] Toggle  [d] Delete "
                .to_string()
        }
    };

    if app.config.indexers.is_empty() {
        let p = Paragraph::new(
            "No Newznab indexers configured.\n\nPress [a] to add an indexer (e.g. NZBgeek) with its URL and API key.",
        )
        .style(theme::subtext())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        );
        frame.render_widget(p, area);
        return;
    }

    let (header, constraints): (Row, Vec<Constraint>) = match mode {
        LayoutMode::Compact => (
            Row::new(vec!["On", "Name", "URL"]).style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Percentage(35),
                Constraint::Percentage(60),
            ],
        ),
        _ => (
            Row::new(vec!["On", "Name", "Endpoint URL", "API Key"]).style(theme::table_header()),
            vec![
                Constraint::Length(4),
                Constraint::Length(18),
                Constraint::Min(20),
                Constraint::Length(14),
            ],
        ),
    };

    let rows: Vec<Row> = app
        .config
        .indexers
        .iter()
        .map(|idx| {
            let mut cells = vec![
                Cell::from(if idx.enabled { "●" } else { "○" }).style(if idx.enabled {
                    theme::success()
                } else {
                    theme::muted()
                }),
                Cell::from(idx.name.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(idx.api_endpoint()).style(theme::subtext()),
            ];
            if mode != LayoutMode::Compact {
                cells.push(Cell::from(mask_secret(&idx.api_key)).style(theme::muted()));
            }
            Row::new(cells)
        })
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.indexer_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_general_settings(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let show_tools = area.height >= 13;
    let (table_area, tools_area) = if show_tools {
        let tools_h = if mode == LayoutMode::Compact { 4 } else { 5 };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(tools_h)])
            .split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };

    let pp = &app.config.downloads.post_process;
    let watch_val = app
        .config
        .downloads
        .watch_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Disabled".to_string());
    let speed_val = if app.config.downloads.speed_limit_kbps == 0 {
        "Unlimited".to_string()
    } else {
        format!("{} KB/s", app.config.downloads.speed_limit_kbps)
    };
    let disk_val = if app.config.downloads.min_free_disk_mb == 0 {
        "Disabled".to_string()
    } else {
        format!("{} MiB", app.config.downloads.min_free_disk_mb)
    };

    let compact = mode == LayoutMode::Compact;
    let items: Vec<(&str, String)> = vec![
        (
            if compact { "Download Dir" } else { "Download Directory" },
            app.config.downloads.download_dir.display().to_string(),
        ),
        (
            if compact { "Temp Dir" } else { "Incomplete / Temp Directory" },
            app.config.downloads.temp_dir.display().to_string(),
        ),
        (
            if compact { "Watch Folder" } else { "Watch Folder (.nzb auto-import)" },
            watch_val,
        ),
        (
            if compact { "Max Jobs" } else { "Max Concurrent Jobs" },
            app.config.downloads.max_concurrent_jobs.to_string(),
        ),
        ("Speed Limit", speed_val),
        (
            if compact { "Min Free Disk" } else { "Min Free Disk Space Guard" },
            disk_val,
        ),
        (
            if compact { "Auto-Export" } else { "Auto-Export Tagged Downloads" },
            bool_badge(app.config.downloads.auto_export),
        ),
        (
            if compact { "Verify PAR2" } else { "Verify PAR2 Checksums" },
            bool_badge(pp.verify_par2),
        ),
        (
            if compact {
                "Repair PAR2"
            } else {
                "Auto-Repair PAR2 (on-demand recovery blocks)"
            },
            bool_badge(pp.repair_par2),
        ),
        (
            if compact {
                "Auto-Extract"
            } else {
                "Auto-Extract Archives (rar/7z/zip/tar)"
            },
            bool_badge(pp.extract_archives),
        ),
        (
            if compact {
                "Cleanup Post"
            } else {
                "Cleanup Archives & PAR2 After Extract"
            },
            bool_badge(pp.cleanup_after_extract),
        ),
        (
            if compact {
                "Job Subfolder"
            } else {
                "Create Subfolder per Download"
            },
            bool_badge(pp.create_job_subfolder),
        ),
    ];

    let rows: Vec<Row> = items
        .into_iter()
        .map(|(k, v)| {
            let v_style = if v == "Enabled" {
                theme::success()
            } else if v == "Disabled" {
                theme::muted()
            } else {
                Style::default().fg(theme::ACCENT)
            };
            Row::new(vec![
                Cell::from(k).style(Style::default().fg(theme::TEXT)),
                Cell::from(v).style(v_style),
            ])
        })
        .collect();

    let cols = if area.width < 80 {
        vec![Constraint::Percentage(52), Constraint::Percentage(48)]
    } else {
        vec![Constraint::Length(44), Constraint::Min(24)]
    };

    let title = if compact {
        " General [Enter/Space] "
    } else {
        " General & Post-Processing — [Enter/Space] Edit or Toggle "
    };

    let table = Table::new(rows, cols)
        .header(Row::new(vec!["Setting", "Value"]).style(theme::table_header()))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.general_selected));
    frame.render_stateful_widget(table, table_area, &mut state);

    if let Some(t_area) = tools_area {
        let t = &app.tools;
        let tool_line = Line::from(vec![
            Span::raw(" par2:"),
            tool_span(t.par2),
            Span::raw(" unrar:"),
            tool_span(t.unrar),
            Span::raw(" 7z:"),
            tool_span(t.seven_zip),
            Span::raw(" unzip:"),
            tool_span(t.unzip),
            Span::raw(" tar:"),
            tool_span(t.tar),
        ]);
        let mut lines = vec![tool_line];
        if !compact {
            lines.push(Line::from(Span::styled(
                " Config file permissions are locked to 0600 (owner read/write only).",
                theme::muted(),
            )));
        }
        let tools_box = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .title(Span::styled(" System Tools ", theme::subtext())),
            );
        frame.render_widget(tools_box, t_area);
    }
}

fn draw_export_targets_settings(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let title = if mode == LayoutMode::Compact {
        " Export Targets [a/e/d] "
    } else {
        " Export Targets (Tag -> Destination Path) — [a] Add  [e] Edit  [d] Delete "
    };

    if app.config.export_targets.is_empty() {
        let p = Paragraph::new(
            "No export targets configured yet.\n\nPress [a] to add a tag and destination directory (e.g. 'Movie' -> '~/Videos/Movies').",
        )
        .style(theme::subtext())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        );
        frame.render_widget(p, area);
        return;
    }

    let header = Row::new(vec!["Tag", "Destination Path"]).style(theme::table_header());
    let rows: Vec<Row> = app
        .config
        .export_targets
        .iter()
        .map(|t| {
            Row::new(vec![
                Cell::from(t.tag.clone()).style(theme::title()),
                Cell::from(t.path.display().to_string()).style(Style::default().fg(theme::TEXT)),
            ])
        })
        .collect();

    let cols = if mode == LayoutMode::Compact {
        vec![Constraint::Percentage(34), Constraint::Percentage(66)]
    } else {
        vec![Constraint::Length(22), Constraint::Min(26)]
    };

    let table = Table::new(rows, cols)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.export_target_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_logs(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let tiny = area.width < 52;
    let lines: Vec<Line> = app
        .snapshot
        .logs
        .iter()
        .map(|entry| {
            let ts = if tiny {
                entry.timestamp.format("%H:%M").to_string()
            } else {
                entry.timestamp.format("%H:%M:%S").to_string()
            };
            let (tag, style) = match entry.level {
                LogLevel::Info => (if tiny { "I" } else { "INFO " }, Style::default().fg(theme::ACCENT_ALT)),
                LogLevel::Warn => (if tiny { "W" } else { "WARN " }, theme::warning()),
                LogLevel::Error => (if tiny { "E" } else { "ERROR" }, theme::danger()),
                LogLevel::Success => (if tiny { "✓" } else { "OK   " }, theme::success()),
            };
            Line::from(vec![
                Span::styled(format!("{} ", ts), theme::muted()),
                Span::styled(format!("[{}] ", tag), style),
                Span::styled(entry.message.clone(), Style::default().fg(theme::TEXT)),
            ])
        })
        .collect();

    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    let scroll = if app.log_auto_scroll {
        max_scroll
    } else {
        app.log_scroll.min(max_scroll)
    };

    let title = if mode == LayoutMode::Compact {
        format!(
            " Logs ({}) [a:{}] ",
            lines.len(),
            if app.log_auto_scroll { "ON" } else { "OFF" }
        )
    } else {
        format!(
            " Activity Log ({}) — Auto-scroll [a]: {}  [G] Bottom ",
            lines.len(),
            if app.log_auto_scroll { "ON" } else { "OFF" }
        )
    };
    let log_widget = Paragraph::new(lines)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        );
    frame.render_widget(log_widget, area);
}

fn draw_export(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let mode_label = if app.export_move_mode { "MOVE" } else { "COPY" };
    let bar_h = if mode == LayoutMode::Compact { 1 } else { 3 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(bar_h), Constraint::Min(5)])
        .split(area);

    if mode == LayoutMode::Compact {
        let line = Line::from(vec![
            Span::styled(format!(" Mode[m]:{} ", mode_label), theme::title()),
            Span::styled(
                format!("| {} target(s)", app.config.export_targets.len()),
                theme::subtext(),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), chunks[0]);
    } else {
        let targets_summary = if app.config.export_targets.is_empty() {
            "No export targets set — go to [3] Settings -> Export Targets -> [a] to add tags & paths."
                .to_string()
        } else {
            app.config
                .export_targets
                .iter()
                .map(|t| format!("{}: {}", t.tag, t.path.display()))
                .collect::<Vec<_>>()
                .join("  |  ")
        };
        let bar = Paragraph::new(Line::from(vec![
            Span::styled(format!(" Mode [m]: {}  ", mode_label), theme::title()),
            Span::styled(targets_summary, theme::subtext()),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(" Configured Export Targets ", theme::subtext())),
        );
        frame.render_widget(bar, chunks[0]);
    }

    let items = app.export_items();
    let title = match mode {
        LayoutMode::Compact => format!(" Export ({}) [t/e/E] ", items.len()),
        LayoutMode::Medium => format!(
            " Export ({}, {}*) — [t] Tag [e] Export [E] All [m] Mode ",
            items.len(),
            app.export_marked.len()
        ),
        LayoutMode::Wide => format!(
            " Download Export ({}, {} marked) — [Space] Mark  [t] Tag  [e/Enter] Export  [E] Export All  [o] Open  [m] Move/Copy ",
            items.len(),
            app.export_marked.len()
        ),
    };

    if items.is_empty() {
        let empty = Paragraph::new(
            "No completed downloads to export yet.\n\nFinished downloads appear here so you can tag and move/copy them to configured targets.",
        )
        .style(theme::subtext())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        );
        frame.render_widget(empty, chunks[1]);
        return;
    }

    let (header, constraints): (Row, Vec<Constraint>) = match mode {
        LayoutMode::Compact => (
            Row::new(vec!["St", "Title", "Tag"]).style(theme::table_header()),
            vec![
                Constraint::Length(6),
                Constraint::Min(14),
                Constraint::Length(9),
            ],
        ),
        LayoutMode::Medium => (
            Row::new(vec!["State", "Title", "Size", "Tag"]).style(theme::table_header()),
            vec![
                Constraint::Length(8),
                Constraint::Min(22),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        ),
        LayoutMode::Wide => (
            Row::new(vec!["State", "Title", "Size", "Tag", "Destination"])
                .style(theme::table_header()),
            vec![
                Constraint::Length(10),
                Constraint::Percentage(36),
                Constraint::Length(10),
                Constraint::Length(14),
                Constraint::Min(24),
            ],
        ),
    };

    let rows: Vec<Row> = items
        .iter()
        .map(|job| {
            let source_exists = job
                .output_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false);

            let (state_label, state_style) = if job.exported_to.is_some() {
                (
                    if mode == LayoutMode::Compact { "DONE" } else { "EXPORTED" },
                    theme::success().add_modifier(Modifier::BOLD),
                )
            } else if !source_exists {
                (
                    if mode == LayoutMode::Compact { "MISS" } else { "MISSING" },
                    theme::danger(),
                )
            } else if let Some(tag) = job.export_tag.as_deref() {
                if app.config.export_path_for_tag(tag).is_some() {
                    ("READY", theme::title())
                } else {
                    ("NOPATH", theme::warning())
                }
            } else {
                (
                    if mode == LayoutMode::Compact { "NONE" } else { "UNTAGGED" },
                    theme::muted(),
                )
            };

            let tag_cell = job
                .export_tag
                .clone()
                .unwrap_or_else(|| "-".to_string());
            let tag_style = if job.export_tag.is_some() {
                theme::title()
            } else {
                theme::muted()
            };

            let dest_cell = if let Some(ref exported) = job.exported_to {
                exported.display().to_string()
            } else if let Some(tag) = job.export_tag.as_deref() {
                app.config
                    .export_path_for_tag(tag)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(no path configured for tag)".to_string())
            } else {
                job.output_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "-".to_string())
            };

            let title_cell = if app.export_marked.contains(&job.id) {
                format!("[*] {}", job.title)
            } else {
                job.title.clone()
            };

            match mode {
                LayoutMode::Compact => Row::new(vec![
                    Cell::from(state_label).style(state_style),
                    Cell::from(title_cell).style(Style::default().fg(theme::TEXT)),
                    Cell::from(tag_cell).style(tag_style),
                ]),
                LayoutMode::Medium => Row::new(vec![
                    Cell::from(state_label).style(state_style),
                    Cell::from(title_cell).style(Style::default().fg(theme::TEXT)),
                    Cell::from(format_bytes(job.total_bytes)).style(theme::subtext()),
                    Cell::from(tag_cell).style(tag_style),
                ]),
                LayoutMode::Wide => Row::new(vec![
                    Cell::from(state_label).style(state_style),
                    Cell::from(title_cell).style(Style::default().fg(theme::TEXT)),
                    Cell::from(format_bytes(job.total_bytes)).style(theme::subtext()),
                    Cell::from(tag_cell).style(tag_style),
                    Cell::from(dest_cell).style(theme::subtext()),
                ]),
            }
        })
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(title, theme::title())),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.export_selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(table, chunks[1], &mut state);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode, very_short: bool) {
    if very_short {
        if let Some(ref toast) = app.toast {
            draw_toast_line(frame, toast.message.as_str(), toast.is_error, area);
        } else {
            let line = Line::from(vec![
                Span::styled(" [1-5]", Style::default().fg(theme::ACCENT)),
                Span::styled("Tab ", theme::muted()),
                Span::styled("[/]", Style::default().fg(theme::ACCENT)),
                Span::styled("Srch ", theme::muted()),
                Span::styled("[?]", Style::default().fg(theme::ACCENT)),
                Span::styled("Help ", theme::muted()),
                Span::styled("[q]", Style::default().fg(theme::ACCENT)),
                Span::styled("Quit", theme::muted()),
            ]);
            frame.render_widget(Paragraph::new(line), area);
        }
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    if let Some(ref toast) = app.toast {
        draw_toast_line(frame, toast.message.as_str(), toast.is_error, chunks[0]);
    } else {
        let hint = if mode == LayoutMode::Compact {
            match app.active_tab {
                ActiveTab::Search => " [/]Srch [c]Cat [S/E] [d]DL [[/]]Pg",
                ActiveTab::Downloads => " [Space]Pause [w]Pane [r]Retry [x]Del",
                ActiveTab::Settings => " [h/l]Sec [a]Add [e]Edit [t]Test [d]Del",
                ActiveTab::Logs => " [j/k]Scroll [G]End [a]Auto",
                ActiveTab::Export => " [t]Tag [e]Exp [E]All [m]Mode",
            }
        } else {
            match app.active_tab {
                ActiveTab::Search => {
                    " [/] Search  [c] Cat  [S] Season  [E] Ep  [[/]] Page  [Space] Mark  [Enter/d] Queue "
                }
                ActiveTab::Downloads => {
                    " [Space] Pause  [P] Pause All  [B] Speed Cap  [r] Retry  [o] Open  [c] Cleanup  [x] Cancel "
                }
                ActiveTab::Settings => {
                    " [h/l] Switch Section  [a] Add  [e] Edit  [t] Test  [Space] Toggle  [d] Delete "
                }
                ActiveTab::Logs => " [j/k] Scroll  [G] Bottom  [a] Toggle Auto-Scroll ",
                ActiveTab::Export => {
                    " [Space] Mark  [t] Tag  [e/Enter] Export  [E] Export All  [o] Open  [m] Move/Copy "
                }
            }
        };
        frame.render_widget(Paragraph::new(hint).style(theme::subtext()), chunks[0]);
    }

    let global_bar = if mode == LayoutMode::Compact {
        Line::from(vec![
            Span::styled(" [1-5]", Style::default().fg(theme::ACCENT)),
            Span::styled("View ", theme::muted()),
            Span::styled("[I]", Style::default().fg(theme::ACCENT)),
            Span::styled("NZB ", theme::muted()),
            Span::styled("[P]", Style::default().fg(theme::ACCENT)),
            Span::styled("Pause ", theme::muted()),
            Span::styled("[?]", Style::default().fg(theme::ACCENT)),
            Span::styled("Help ", theme::muted()),
            Span::styled("[q]", Style::default().fg(theme::ACCENT)),
            Span::styled("Quit", theme::muted()),
        ])
    } else {
        Line::from(vec![
            Span::styled(" [1-5/Tab]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Views  ", theme::muted()),
            Span::styled("[I]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Import NZB  ", theme::muted()),
            Span::styled("[P]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Global Pause  ", theme::muted()),
            Span::styled("[U]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Update  ", theme::muted()),
            Span::styled("[?]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Help  ", theme::muted()),
            Span::styled("[q]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Quit", theme::muted()),
        ])
    };
    frame.render_widget(Paragraph::new(global_bar), chunks[1]);
}

fn draw_toast_line(frame: &mut Frame, message: &str, is_error: bool, area: Rect) {
    let style = if is_error {
        Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    };
    frame.render_widget(Paragraph::new(format!(" {} ", message)).style(style), area);
}

fn draw_form_modal(
    frame: &mut Frame,
    title: &str,
    fields: Vec<(&str, String, bool)>,
    active_field: usize,
    desired_w: u16,
    label_col_w: usize,
    area: Rect,
) {
    let stacked = area.width < 62;
    let desired_h = if stacked {
        (fields.len() * 2 + 5) as u16
    } else {
        (fields.len() + 5) as u16
    };
    let popup = centered_rect(desired_w, desired_h, area);
    frame.render_widget(Clear, popup);

    let inner_w = popup.width.saturating_sub(2) as usize;
    let inner_h = popup.height.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    let mut active_line_start = 0usize;

    for (idx, (label, val, is_bool)) in fields.into_iter().enumerate() {
        let active = idx == active_field;
        if active {
            active_line_start = lines.len();
        }
        let cursor = if active && !is_bool { "▌" } else { "" };
        let marker = if active { "▸ " } else { "  " };
        let label_style = if active { theme::title() } else { theme::subtext() };
        let val_style = if active {
            Style::default().fg(theme::TEXT).bg(theme::HIGHLIGHT_BG)
        } else {
            Style::default().fg(theme::TEXT)
        };

        if stacked {
            let max_val_w = inner_w.saturating_sub(5);
            let shown_val = if active {
                tail_fit(&val, max_val_w)
            } else {
                head_fit(&val, max_val_w)
            };
            lines.push(Line::from(Span::styled(
                format!("{}{}", marker, label),
                label_style,
            )));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!(" {}{} ", shown_val, cursor), val_style),
            ]));
        } else {
            let lw = label_col_w.min(inner_w.saturating_sub(14));
            let max_val_w = inner_w.saturating_sub(lw + 6);
            let shown_val = if active {
                tail_fit(&val, max_val_w)
            } else {
                head_fit(&val, max_val_w)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{}{:<width$} ", marker, label, width = lw), label_style),
                Span::styled(format!(" {}{} ", shown_val, cursor), val_style),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if stacked {
            " [Tab/↑↓]Field [Enter]Save [Esc]Cancel"
        } else {
            "  [Tab/↑↓] Next/Prev Field   [Enter] Save   [Esc] Cancel"
        },
        theme::muted(),
    )));

    // Keep active field comfortably visible when modal is vertically constrained (e.g. mobile keyboard)
    let scroll_y = if lines.len() > inner_h && inner_h > 2 {
        let target = active_line_start.saturating_sub(inner_h / 3);
        target.min(lines.len().saturating_sub(inner_h)) as u16
    } else {
        0
    };

    let widget = Paragraph::new(lines)
        .scroll((scroll_y, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .style(Style::default().bg(theme::MODAL_BG))
                .title(Span::styled(title, theme::title())),
        );
    frame.render_widget(widget, popup);
}

fn draw_provider_modal(frame: &mut Frame, form: &ProviderForm, area: Rect) {
    let title = if form.edit_index.is_some() {
        " Edit Provider "
    } else {
        " Add Provider "
    };

    let pw_display = if form.show_password {
        form.password.clone()
    } else {
        "•".repeat(form.password.chars().count())
    };

    let compact = area.width < 62;
    let fields: Vec<(&str, String, bool)> = vec![
        ("Name", form.name.clone(), false),
        ("Host", form.host.clone(), false),
        ("Port", form.port.clone(), false),
        (
            if compact { "TLS [Space]" } else { "TLS (Space toggle)" },
            if form.tls { "Yes (NNTPS)".into() } else { "No (Plain)".into() },
            true,
        ),
        ("Username", form.username.clone(), false),
        (
            if compact { "Password [Ctrl+R]" } else { "Password (Ctrl+R reveal)" },
            pw_display,
            false,
        ),
        (
            if compact { "Connections" } else { "Connections (1–100)" },
            form.connections.clone(),
            false,
        ),
        (
            if compact { "Priority (0=Pri)" } else { "Priority (0=Primary, 1+=Backup)" },
            form.priority.clone(),
            false,
        ),
        (
            if compact { "Enabled [Space]" } else { "Enabled (Space toggle)" },
            if form.enabled { "Yes".into() } else { "No".into() },
            true,
        ),
    ];

    draw_form_modal(frame, title, fields, form.field, 64, 32, area);
}

fn draw_indexer_modal(frame: &mut Frame, form: &IndexerForm, area: Rect) {
    let title = if form.edit_index.is_some() {
        " Edit Indexer "
    } else {
        " Add Indexer "
    };

    let key_display = if form.show_key {
        form.api_key.clone()
    } else {
        "•".repeat(form.api_key.chars().count())
    };

    let compact = area.width < 62;
    let fields: Vec<(&str, String, bool)> = vec![
        ("Name", form.name.clone(), false),
        (
            if compact { "URL" } else { "URL (e.g. https://api.nzbgeek.info)" },
            form.url.clone(),
            false,
        ),
        (
            if compact { "API Key [Ctrl+R]" } else { "API Key (Ctrl+R reveal)" },
            key_display,
            false,
        ),
        (
            if compact { "Enabled [Space]" } else { "Enabled (Space toggle)" },
            if form.enabled { "Yes".into() } else { "No".into() },
            true,
        ),
    ];

    draw_form_modal(frame, title, fields, form.field, 68, 36, area);
}

fn draw_export_target_modal(frame: &mut Frame, form: &ExportTargetForm, area: Rect) {
    let title = if form.edit_index.is_some() {
        " Edit Export Target "
    } else {
        " Add Export Target "
    };

    let compact = area.width < 62;
    let fields = vec![
        (
            if compact { "Tag (Movie/TV)" } else { "Tag Name (e.g. Movie, TV)" },
            form.tag.clone(),
            false,
        ),
        (
            if compact { "Destination Dir" } else { "Destination Directory" },
            form.path.clone(),
            false,
        ),
    ];

    draw_form_modal(frame, title, fields, form.field, 64, 28, area);
}

fn draw_text_prompt_modal(frame: &mut Frame, prompt: &TextPrompt, area: Rect) {
    let popup = centered_rect(66, 9, area);
    frame.render_widget(Clear, popup);

    let max_val_w = popup.width.saturating_sub(6) as usize;
    let shown = tail_fit(&prompt.value, max_val_w);

    let lines = vec![
        Line::from(Span::styled(format!(" {}", prompt.hint), theme::subtext())),
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}▌ ", shown),
            Style::default().fg(theme::TEXT).bg(theme::HIGHLIGHT_BG),
        )),
        Line::from(""),
        Line::from(Span::styled(
            if popup.width < 52 {
                " [Enter]OK [Ctrl+U]Clr [Esc]No"
            } else {
                " [Enter] Confirm   [Ctrl+U] Clear   [Esc] Cancel"
            },
            theme::muted(),
        )),
    ];

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .style(Style::default().bg(theme::MODAL_BG))
                .title(Span::styled(format!(" {} ", prompt.title), theme::title())),
        );
    frame.render_widget(widget, popup);
}

fn draw_job_details_modal(frame: &mut Frame, job: &JobSummary, scroll: u16, area: Rect) {
    let popup = centered_rect(72, 16, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Title:    ", theme::subtext()),
            Span::styled(
                job.title.clone(),
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Status:   ", theme::subtext()),
            Span::styled(job.status.label(), status_style(job.status)),
            Span::styled(format!(" ({})", job.stage_detail), theme::muted()),
        ]),
        Line::from(vec![
            Span::styled(" Progress: ", theme::subtext()),
            Span::styled(
                format!(
                    "{}/{} ({:.1}%)",
                    format_bytes(job.downloaded_bytes),
                    format_bytes(job.total_bytes),
                    job.progress_ratio() * 100.0
                ),
                Style::default().fg(theme::ACCENT),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Segments: ", theme::subtext()),
            Span::raw(format!(
                "{}/{} ({} miss)",
                job.completed_segments, job.total_segments, job.missing_segments
            )),
        ]),
        Line::from(vec![
            Span::styled(" Files:    ", theme::subtext()),
            Span::raw(format!("{}/{}", job.files_done, job.files_total)),
        ]),
    ];

    if let Some(ref cat) = job.category {
        lines.push(Line::from(vec![
            Span::styled(" Category: ", theme::subtext()),
            Span::raw(cat.clone()),
        ]));
    }
    if let Some(ref path) = job.output_path {
        lines.push(Line::from(vec![
            Span::styled(" Output:   ", theme::subtext()),
            Span::styled(path.display().to_string(), theme::success()),
        ]));
    }
    if let Some(ref err) = job.error {
        lines.push(Line::from(vec![
            Span::styled(" Error:    ", theme::subtext()),
            Span::styled(err.clone(), theme::danger()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " [j/k] Scroll   [Esc/Enter] Close",
        theme::muted(),
    )));

    let widget = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .style(Style::default().bg(theme::MODAL_BG))
                .title(Span::styled(" Job Details ", theme::title())),
        );
    frame.render_widget(widget, popup);
}

fn draw_help_modal(frame: &mut Frame, scroll: u16, area: Rect) {
    let popup = centered_rect(72, 29, area);
    frame.render_widget(Clear, popup);

    let compact = popup.width < 58;
    let lines = if compact {
        vec![
            Line::from(Span::styled(" Global", theme::title())),
            Line::from(" 1-5 / Tab  Switch views"),
            Line::from(" I / U      Import .nzb / Update"),
            Line::from(" P / ? / q  Pause All / Help / Quit"),
            Line::from(""),
            Line::from(Span::styled(" Search", theme::title())),
            Line::from(" / or s     Focus search"),
            Line::from(" c / S / E  Cat / Season / Episode"),
            Line::from(" [ / ]      Prev / Next page"),
            Line::from(" i / o      Indexer / Sort"),
            Line::from(" Space      Mark multiple"),
            Line::from(" Enter / d  Download"),
            Line::from(""),
            Line::from(Span::styled(" Downloads", theme::title())),
            Line::from(" w          Switch Queue/History"),
            Line::from(" Space / B  Pause / Speed cap"),
            Line::from(" K / J      Reorder priority"),
            Line::from(" r / o / x  Retry / Open / Del"),
            Line::from(" c / C      Cleanup / Clear all"),
            Line::from(""),
            Line::from(Span::styled(" Settings & Export", theme::title())),
            Line::from(" h / l      Switch section"),
            Line::from(" a/e/d/t    Add/Edit/Del/Test"),
            Line::from(" t/e/E/m    Tag/Export/All/Mode"),
        ]
    } else {
        vec![
            Line::from(Span::styled(" Global Keys", theme::title())),
            Line::from("   1 / 2 / 3 / 4 / 5   Jump to Search / Downloads / Settings / Logs / Export"),
            Line::from("   Tab / Shift+Tab     Cycle through all views & sub-panes"),
            Line::from("   I / U               Import local .nzb | Check & install update"),
            Line::from("   P                   Pause / resume all downloads"),
            Line::from("   ? / q / Ctrl+C      Help / Quit"),
            Line::from(""),
            Line::from(Span::styled(" Search View", theme::title())),
            Line::from("   / or s              Focus search bar (Enter to run, Esc to blur)"),
            Line::from("   c / C | S / E       Cycle category | Filter by Season (00=packs) / Episode"),
            Line::from("   [ / ] | i / o       Prev/Next results page | Cycle indexer / sort order"),
            Line::from("   Space | Enter / d   Mark multiple | Queue selected or marked NZBs"),
            Line::from(""),
            Line::from(Span::styled(" Downloads View", theme::title())),
            Line::from("   Space / p | B       Pause or resume job | Set download speed limit"),
            Line::from("   x / Delete | r | o  Cancel/Remove | Retry failed | Open output folder"),
            Line::from("   K / J               Move queued job priority up / down"),
            Line::from("   w / Enter           Switch Queue/History | Details"),
            Line::from("   c / C               Cleanup failed & temp dirs | Clear all history"),
            Line::from(""),
            Line::from(Span::styled(" Settings & Export Views", theme::title())),
            Line::from("   h / l               Switch Providers / Indexers / General / Export Targets"),
            Line::from("   a / e / d / t       Add / Edit / Delete / Live Test Connection"),
            Line::from("   Space / t / e / E   Export: Mark / Tag / Export / Export All / [o] Open / [m] Mode"),
        ]
    };

    let title = if popup.height < 26 {
        " Keys [j/k]Scroll [Esc]Close "
    } else {
        " nzbtui Keybindings — [j/k] Scroll  [Esc] Close "
    };

    let widget = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .style(Style::default().bg(theme::MODAL_BG))
                .title(Span::styled(title, theme::title())),
        );
    frame.render_widget(widget, popup);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let margin_w = if area.width < 52 { 0 } else { 2 };
    let margin_h = if area.height < 18 { 0 } else { 2 };
    let w = width.min(area.width.saturating_sub(margin_w)).max(1);
    let h = height.min(area.height.saturating_sub(margin_h)).max(1);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn tail_fit(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    if max_chars <= 2 {
        return chars[chars.len() - max_chars..].iter().collect();
    }
    let tail: String = chars[chars.len() - (max_chars - 1)..].iter().collect();
    format!("…{}", tail)
}

fn head_fit(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    if max_chars <= 2 {
        return chars[..max_chars].iter().collect();
    }
    let head: String = chars[..(max_chars - 1)].iter().collect();
    format!("{}…", head)
}

fn status_style(status: JobStatus) -> Style {
    match status {
        JobStatus::Downloading | JobStatus::DownloadingRepair => {
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
        }
        JobStatus::Verifying | JobStatus::Repairing | JobStatus::Extracting | JobStatus::Finalizing => {
            Style::default().fg(theme::WARNING).add_modifier(Modifier::BOLD)
        }
        JobStatus::Completed => theme::success().add_modifier(Modifier::BOLD),
        JobStatus::Failed => theme::danger().add_modifier(Modifier::BOLD),
        JobStatus::Paused => theme::warning(),
        JobStatus::Queued | JobStatus::Cancelled => theme::muted(),
    }
}

fn ascii_progress_bar(ratio: f64, width: usize) -> String {
    let filled = ((ratio.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn bool_badge(v: bool) -> String {
    if v {
        "Enabled".to_string()
    } else {
        "Disabled".to_string()
    }
}

fn tool_span(available: bool) -> Span<'static> {
    if available {
        Span::styled("OK", theme::success())
    } else {
        Span::styled("missing", theme::warning())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, IndexerConfig, ProviderConfig};
    use crate::engine::DownloadEngine;
    use crate::newznab::NewznabClient;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_render_responsive_mobile_and_desktop_viewports() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(ProviderConfig::new_template());
        cfg.indexers.push(IndexerConfig::new_template());
        let cfg_arc = Arc::new(RwLock::new(cfg.clone()));
        let state_dir = std::env::temp_dir().join(format!("nzbtui_ui_test_{}", uuid::Uuid::new_v4()));
        let engine = DownloadEngine::new(Arc::clone(&cfg_arc), state_dir.clone());
        let client = Arc::new(NewznabClient::new().unwrap());
        let snap = engine.snapshot().await;
        let mut app = App::new(
            cfg,
            cfg_arc,
            PathBuf::from("/tmp/nzbtui_test.json"),
            Arc::clone(&engine),
            client,
            snap,
        );

        let viewports = [
            (38u16, 14u16), // iPhone iSH portrait with keyboard open
            (48, 24),       // iPhone iSH portrait
            (76, 22),       // Landscape phone / tmux split
            (120, 36),      // Desktop terminal
        ];

        for (w, h) in viewports {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();

            for tab in [
                ActiveTab::Search,
                ActiveTab::Downloads,
                ActiveTab::Settings,
                ActiveTab::Logs,
                ActiveTab::Export,
            ] {
                app.active_tab = tab;
                app.modal = None;
                terminal.draw(|f| draw(f, &app)).unwrap();
            }

            // Render settings sub-sections
            app.active_tab = ActiveTab::Settings;
            for sec in [
                SettingsSection::Providers,
                SettingsSection::Indexers,
                SettingsSection::General,
                SettingsSection::Export,
            ] {
                app.settings_section = sec;
                terminal.draw(|f| draw(f, &app)).unwrap();
            }

            // Render modals at mobile and desktop widths
            let modals = [
                Modal::Help,
                Modal::Provider(ProviderForm::from_config(&ProviderConfig::new_template(), None)),
                Modal::Indexer(IndexerForm::from_config(&IndexerConfig::new_template(), None)),
                Modal::ExportTarget(ExportTargetForm::new_empty()),
            ];
            for m in modals {
                app.modal = Some(m);
                terminal.draw(|f| draw(f, &app)).unwrap();
            }
        }

        engine.shutdown().await;
        let _ = std::fs::remove_dir_all(&state_dir);
    }
}
