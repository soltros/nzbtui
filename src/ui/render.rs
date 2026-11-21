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

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header / Tabs / Live speed
            Constraint::Min(8),    // Main content
            Constraint::Length(2), // Toast + Footer keybind hints
        ])
        .split(area);

    draw_header(frame, app, outer[0]);

    match app.active_tab {
        ActiveTab::Search => draw_search(frame, app, outer[1]),
        ActiveTab::Downloads => draw_downloads(frame, app, outer[1]),
        ActiveTab::Settings => draw_settings(frame, app, outer[1]),
        ActiveTab::Logs => draw_logs(frame, app, outer[1]),
        ActiveTab::Export => draw_export(frame, app, outer[1]),
    }

    draw_footer(frame, app, outer[2]);

    if let Some(ref modal) = app.modal {
        match modal {
            Modal::Help => draw_help_modal(frame, area),
            Modal::Provider(form) => draw_provider_modal(frame, form, area),
            Modal::Indexer(form) => draw_indexer_modal(frame, form, area),
            Modal::ExportTarget(form) => draw_export_target_modal(frame, form, area),
            Modal::TextPrompt(prompt) => draw_text_prompt_modal(frame, prompt, area),
            Modal::JobDetails(job) => draw_job_details_modal(frame, job, area),
        }
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(40), Constraint::Length(38)])
        .split(area);

    let queue_badge = if app.snapshot.queue.is_empty() {
        "2:Downloads".to_string()
    } else {
        format!("2:Downloads ({})", app.snapshot.queue.len())
    };

    let tab_titles = vec![
        Line::from(" 1:Search "),
        Line::from(format!(" {} ", queue_badge)),
        Line::from(" 3:Settings "),
        Line::from(" 4:Logs "),
        Line::from(" 5:Export "),
    ];

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

    // Right status pill: speed + active connections + pause state
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
                "  [{}/{} conn]",
                app.snapshot.active_connections, app.snapshot.max_connections
            ),
            theme::subtext(),
        ),
    ]);

    let status_widget = Paragraph::new(status_line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(" Status ", theme::subtext())),
    );
    frame.render_widget(status_widget, chunks[1]);
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    // Search bar + filters
    let bar_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(24),
            Constraint::Length(16),
            Constraint::Length(16),
            Constraint::Length(18),
            Constraint::Length(12),
        ])
        .split(chunks[0]);

    let query_display = if app.search_input_focused {
        format!("{}▌", app.search_query)
    } else if app.search_query.is_empty() {
        "Press [/] to search Newznab indexers (leave empty for latest)…".to_string()
    } else {
        app.search_query.clone()
    };

    let query_style = if app.search_input_focused {
        Style::default().fg(theme::TEXT)
    } else if app.search_query.is_empty() {
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
    frame.render_widget(search_input, bar_chunks[0]);

    let cat_label = CATEGORIES[app.search_category_idx].label;
    let cat_box = Paragraph::new(cat_label)
        .style(Style::default().fg(theme::TEXT))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(" Cat [c] ", theme::subtext())),
        );
    frame.render_widget(cat_box, bar_chunks[1]);

    let season_label = if app.search_season.trim().is_empty() {
        "All".to_string()
    } else if app.search_season.trim() == "00" {
        "00 (Packs)".to_string()
    } else {
        format!("S{}", app.search_season.trim())
    };
    let season_style = if app.search_season.trim().is_empty() {
        Style::default().fg(theme::TEXT)
    } else {
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD)
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

    let idx_box = Paragraph::new(app.current_indexer_filter_label())
        .style(Style::default().fg(theme::TEXT))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(" Indexer [i] ", theme::subtext())),
        );
    frame.render_widget(idx_box, bar_chunks[3]);

    let sort_box = Paragraph::new(app.search_sort.label())
        .style(Style::default().fg(theme::TEXT))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(" Sort [o] ", theme::subtext())),
        );
    frame.render_widget(sort_box, bar_chunks[4]);

    // Results table
    if app.search_results.is_empty() {
        let msg = if app.search_loading {
            "Querying Newznab indexers…".to_string()
        } else if app.config.enabled_indexers().is_empty() {
            "No Newznab indexers configured yet.\n\nPress [3] to open Settings, move right [l] to Indexers, and press [a] to add an indexer URL and API key.\nOr press [I] anywhere to import a local .nzb file.".to_string()
        } else {
            "No results yet. Press [/] to type a search query and hit [Enter], or press [r] to fetch latest releases.".to_string()
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
        frame.render_widget(empty, chunks[1]);
        return;
    }

    let header = Row::new(vec!["Title", "Size", "Age", "Grabs", "Indexer", "Category"])
        .style(theme::table_header())
        .bottom_margin(0);

    let rows: Vec<Row> = app
        .search_results
        .iter()
        .map(|item| {
            let queued_prefix = if app.queued_guids.contains(&item.guid) {
                "[✓] "
            } else if item.password_protected {
                "[🔒] "
            } else {
                ""
            };
            let title_text = format!("{}{}", queued_prefix, item.title);
            let title_style = if app.queued_guids.contains(&item.guid) {
                theme::success()
            } else {
                Style::default().fg(theme::TEXT)
            };

            Row::new(vec![
                Cell::from(title_text).style(title_style),
                Cell::from(format_bytes(item.size)).style(Style::default().fg(theme::ACCENT)),
                Cell::from(item.age_display()).style(theme::subtext()),
                Cell::from(item.grabs.to_string()).style(theme::subtext()),
                Cell::from(item.indexer_name.clone()).style(theme::subtext()),
                Cell::from(item.category.clone()).style(theme::muted()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(36),
            Constraint::Length(11),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Length(18),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(if !app.search_input_focused {
                theme::border_focused()
            } else {
                theme::border()
            })
            .title(Span::styled(
                format!(
                    " Results ({}) — [Enter/d] Download  [I] Import .nzb ",
                    app.search_results.len()
                ),
                theme::title(),
            )),
    )
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.search_selected));
    frame.render_stateful_widget(table, chunks[1], &mut state);
}

fn draw_downloads(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),      // Sparkline throughput graph
            Constraint::Percentage(55), // Active queue
            Constraint::Percentage(45), // History
        ])
        .split(area);

    // Sparkline throughput bar
    let spark_data: Vec<u64> = app
        .snapshot
        .speed_history
        .iter()
        .map(|&bps| bps / 1024) // KiB/s scale
        .collect();
    let peak_bps = app
        .snapshot
        .speed_history
        .iter()
        .copied()
        .max()
        .unwrap_or(0);

    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(
                    format!(
                        " Throughput (60s) — Now: {} | Peak: {} | Connections: {}/{} ",
                        format_speed(app.snapshot.current_speed_bps),
                        format_speed(peak_bps),
                        app.snapshot.active_connections,
                        app.snapshot.max_connections
                    ),
                    theme::title(),
                )),
        )
        .data(&spark_data)
        .style(Style::default().fg(theme::ACCENT));
    frame.render_widget(sparkline, chunks[0]);

    // Active Queue Table
    let queue_focused = app.downloads_pane == DownloadsPane::Queue;
    let q_header = Row::new(vec!["State", "Title", "Progress", "Size", "ETA", "Detail"])
        .style(theme::table_header());

    let q_rows: Vec<Row> = app
        .snapshot
        .queue
        .iter()
        .map(|job| {
            let pct = (job.progress_ratio() * 100.0).round() as u16;
            let bar = ascii_progress_bar(job.progress_ratio(), 12);
            let prog_str = format!("{} {:3}%", bar, pct);
            Row::new(vec![
                Cell::from(job.status.label()).style(status_style(job.status)),
                Cell::from(job.title.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(prog_str).style(Style::default().fg(theme::ACCENT)),
                Cell::from(format!(
                    "{}/{}",
                    format_bytes(job.downloaded_bytes),
                    format_bytes(job.total_bytes)
                ))
                .style(theme::subtext()),
                Cell::from(format_eta(job.eta_seconds())).style(theme::subtext()),
                Cell::from(job.stage_detail.clone()).style(theme::muted()),
            ])
        })
        .collect();

    let queue_table = Table::new(
        q_rows,
        [
            Constraint::Length(9),
            Constraint::Min(26),
            Constraint::Length(18),
            Constraint::Length(20),
            Constraint::Length(8),
            Constraint::Length(26),
        ],
    )
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
                format!(
                    " Active Queue ({}) — [Space] Pause  [x] Cancel  [K/J] Reorder  [w] Switch Pane ",
                    app.snapshot.queue.len()
                ),
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
    frame.render_stateful_widget(queue_table, chunks[1], &mut q_state);

    // History Table
    let hist_focused = app.downloads_pane == DownloadsPane::History;
    let h_header = Row::new(vec!["Result", "Title", "Size", "Output / Error"])
        .style(theme::table_header());

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
            Row::new(vec![
                Cell::from(job.status.label()).style(status_style(job.status)),
                Cell::from(job.title.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(format_bytes(job.total_bytes)).style(theme::subtext()),
                Cell::from(info).style(if job.status == JobStatus::Failed {
                    theme::danger()
                } else {
                    theme::muted()
                }),
            ])
        })
        .collect();

    let hist_table = Table::new(
        h_rows,
        [
            Constraint::Length(9),
            Constraint::Min(28),
            Constraint::Length(12),
            Constraint::Min(28),
        ],
    )
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
                format!(
                    " History ({}) — [Enter] Details  [x] Remove  [c] Cleanup  [C] Clear All ",
                    app.snapshot.history.len()
                ),
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
    frame.render_stateful_widget(hist_table, chunks[2], &mut h_state);
}

fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8)])
        .split(area);

    let sub_titles = vec![
        Line::from(format!(" Usenet Providers ({}) ", app.config.providers.len())),
        Line::from(format!(" Newznab Indexers ({}) ", app.config.indexers.len())),
        Line::from(" General & Post-Processing "),
        Line::from(format!(
            " Export Targets ({}) ",
            app.config.export_targets.len()
        )),
    ];
    let sub_idx = match app.settings_section {
        SettingsSection::Providers => 0,
        SettingsSection::Indexers => 1,
        SettingsSection::General => 2,
        SettingsSection::Export => 3,
    };

    let sub_tabs = Tabs::new(sub_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(
                    format!(
                        " Settings [h/l switch section] — Config: {} ",
                        app.config_path.display()
                    ),
                    theme::subtext(),
                )),
        )
        .select(sub_idx)
        .style(theme::subtext())
        .highlight_style(theme::title());
    frame.render_widget(sub_tabs, chunks[0]);

    match app.settings_section {
        SettingsSection::Providers => draw_providers_settings(frame, app, chunks[1]),
        SettingsSection::Indexers => draw_indexers_settings(frame, app, chunks[1]),
        SettingsSection::General => draw_general_settings(frame, app, chunks[1]),
        SettingsSection::Export => draw_export_targets_settings(frame, app, chunks[1]),
    }
}

fn draw_providers_settings(frame: &mut Frame, app: &App, area: Rect) {
    let title_suffix = app
        .testing_status
        .as_deref()
        .map(|s| format!(" — {} ", s))
        .unwrap_or_else(|| {
            " — [a] Add  [e] Edit  [t] Test Connection  [Space] Toggle  [d] Delete ".to_string()
        });

    if app.config.providers.is_empty() {
        let p = Paragraph::new(
            "No Usenet providers configured.\n\nPress [a] to add your primary NNTP server (host, port 563 TLS, username, password, connections, priority 0).\nYou can also add backup/fill providers with priority 1+ for automatic missing-article fallback.",
        )
        .style(theme::subtext())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(
                    format!(" Usenet Providers{} ", title_suffix),
                    theme::title(),
                )),
        );
        frame.render_widget(p, area);
        return;
    }

    let header = Row::new(vec![
        "On",
        "Pri",
        "Name",
        "Host:Port",
        "TLS",
        "Username",
        "Password",
        "Conns",
    ])
    .style(theme::table_header());

    let rows: Vec<Row> = app
        .config
        .providers
        .iter()
        .map(|p| {
            Row::new(vec![
                Cell::from(if p.enabled { "●" } else { "○" }).style(if p.enabled {
                    theme::success()
                } else {
                    theme::muted()
                }),
                Cell::from(p.priority.to_string()).style(Style::default().fg(theme::ACCENT)),
                Cell::from(p.name.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(format!("{}:{}", p.host, p.port)).style(theme::subtext()),
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
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(5),
            Constraint::Length(18),
            Constraint::Min(24),
            Constraint::Length(7),
            Constraint::Length(18),
            Constraint::Length(12),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .title(Span::styled(
                format!(" Usenet Providers{} ", title_suffix),
                theme::title(),
            )),
    )
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.provider_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_indexers_settings(frame: &mut Frame, app: &App, area: Rect) {
    let title_suffix = app
        .testing_status
        .as_deref()
        .map(|s| format!(" — {} ", s))
        .unwrap_or_else(|| {
            " — [a] Add  [e] Edit  [t] Test API Key  [Space] Toggle  [d] Delete ".to_string()
        });

    if app.config.indexers.is_empty() {
        let p = Paragraph::new(
            "No Newznab indexers configured.\n\nPress [a] to add an indexer (e.g. NZBgeek, DrunkenSlug, NZBfinder, NZBPlanet, etc.) with its base URL and API key.",
        )
        .style(theme::subtext())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(
                    format!(" Newznab Indexers{} ", title_suffix),
                    theme::title(),
                )),
        );
        frame.render_widget(p, area);
        return;
    }

    let header = Row::new(vec!["On", "Name", "Endpoint URL", "API Key"]).style(theme::table_header());

    let rows: Vec<Row> = app
        .config
        .indexers
        .iter()
        .map(|idx| {
            Row::new(vec![
                Cell::from(if idx.enabled { "●" } else { "○" }).style(if idx.enabled {
                    theme::success()
                } else {
                    theme::muted()
                }),
                Cell::from(idx.name.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(idx.api_endpoint()).style(theme::subtext()),
                Cell::from(mask_secret(&idx.api_key)).style(theme::muted()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(20),
            Constraint::Min(32),
            Constraint::Length(16),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .title(Span::styled(
                format!(" Newznab Indexers{} ", title_suffix),
                theme::title(),
            )),
    )
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.indexer_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_general_settings(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(11), Constraint::Length(5)])
        .split(area);

    let pp = &app.config.downloads.post_process;
    let items: Vec<(&str, String)> = vec![
        (
            "Download Directory",
            app.config.downloads.download_dir.display().to_string(),
        ),
        (
            "Incomplete / Temp Directory",
            app.config.downloads.temp_dir.display().to_string(),
        ),
        (
            "Max Concurrent Jobs",
            app.config.downloads.max_concurrent_jobs.to_string(),
        ),
        ("Verify PAR2 Checksums", bool_badge(pp.verify_par2)),
        (
            "Auto-Repair PAR2 (on-demand recovery blocks)",
            bool_badge(pp.repair_par2),
        ),
        (
            "Auto-Extract Archives (rar/7z/zip/tar)",
            bool_badge(pp.extract_archives),
        ),
        (
            "Cleanup Archives & PAR2 After Extract",
            bool_badge(pp.cleanup_after_extract),
        ),
        (
            "Create Subfolder per Download",
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

    let table = Table::new(rows, [Constraint::Length(44), Constraint::Min(24)])
        .header(Row::new(vec!["Setting", "Value"]).style(theme::table_header()))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_focused())
                .title(Span::styled(
                    " General & Post-Processing — [Enter/Space] Edit or Toggle ",
                    theme::title(),
                )),
        )
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("▸ ");

    let mut state = TableState::default();
    state.select(Some(app.general_selected));
    frame.render_stateful_widget(table, chunks[0], &mut state);

    // Detected system tools box
    let t = &app.tools;
    let tool_line = Line::from(vec![
        Span::raw("  par2: "),
        tool_span(t.par2),
        Span::raw("   unrar: "),
        tool_span(t.unrar),
        Span::raw("   7z: "),
        tool_span(t.seven_zip),
        Span::raw("   unzip: "),
        tool_span(t.unzip),
        Span::raw("   tar: "),
        tool_span(t.tar),
    ]);
    let tools_box = Paragraph::new(vec![
        tool_line,
        Line::from(Span::styled(
            "  Config file permissions are locked to 0600 (owner read/write only).",
            theme::muted(),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(" System Post-Processing Tools ", theme::subtext())),
    );
    frame.render_widget(tools_box, chunks[1]);
}

fn draw_export_targets_settings(frame: &mut Frame, app: &App, area: Rect) {
    let title = " Export Targets (Tag -> Destination Path) — [a] Add  [e] Edit  [d] Delete ";

    if app.config.export_targets.is_empty() {
        let p = Paragraph::new(
            "No export targets configured yet.\n\nPress [a] to add a tag and destination directory (for example, tag 'Movie' -> '~/Videos/Movies').\nThen in [5:Export], press [t] to tag completed downloads and [e] to export them.",
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
                Cell::from(t.tag.clone()).style(
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(t.path.display().to_string()).style(Style::default().fg(theme::TEXT)),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(22), Constraint::Min(30)])
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

fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .snapshot
        .logs
        .iter()
        .map(|entry| {
            let ts = entry.timestamp.format("%H:%M:%S").to_string();
            let (tag, style) = match entry.level {
                LogLevel::Info => ("INFO ", Style::default().fg(theme::ACCENT_ALT)),
                LogLevel::Warn => ("WARN ", theme::warning()),
                LogLevel::Error => ("ERROR", theme::danger()),
                LogLevel::Success => ("OK   ", theme::success()),
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

    let title = format!(
        " Activity Log ({}) — Auto-scroll [a]: {}  [G] Bottom ",
        lines.len(),
        if app.log_auto_scroll { "ON" } else { "OFF" }
    );
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

fn draw_export(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    // Top bar: configured export targets + Move/Copy mode
    let mode_label = if app.export_move_mode { "MOVE" } else { "COPY" };
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
        Span::styled(
            format!(" Mode [m]: {}  ", mode_label),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(targets_summary, theme::subtext()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(" Configured Export Targets ", theme::subtext())),
    );
    frame.render_widget(bar, chunks[0]);

    let items = app.export_items();
    let title = format!(
        " Download Export ({}) — [t] Cycle Tag  [e/Enter] Export  [E] Export All Tagged  [m] Move/Copy ",
        items.len()
    );

    if items.is_empty() {
        let empty = Paragraph::new(
            "No completed downloads available to export yet.\n\nFinished downloads will appear here so you can tag them (e.g. Movie, TV) and export them to your configured directories.",
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

    let header = Row::new(vec![
        "State",
        "Title",
        "Size",
        "Tag",
        "Destination",
    ])
    .style(theme::table_header());

    let rows: Vec<Row> = items
        .iter()
        .map(|job| {
            let source_exists = job
                .output_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false);

            let (state_label, state_style) = if job.exported_to.is_some() {
                ("EXPORTED", theme::success().add_modifier(Modifier::BOLD))
            } else if !source_exists {
                ("MISSING", theme::danger())
            } else if let Some(tag) = job.export_tag.as_deref() {
                if app.config.export_path_for_tag(tag).is_some() {
                    ("READY", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD))
                } else {
                    ("NO PATH", theme::warning())
                }
            } else {
                ("UNTAGGED", theme::muted())
            };

            let tag_cell = job
                .export_tag
                .clone()
                .unwrap_or_else(|| "-".to_string());
            let tag_style = if job.export_tag.is_some() {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
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

            Row::new(vec![
                Cell::from(state_label).style(state_style),
                Cell::from(job.title.clone()).style(Style::default().fg(theme::TEXT)),
                Cell::from(format_bytes(job.total_bytes)).style(theme::subtext()),
                Cell::from(tag_cell).style(tag_style),
                Cell::from(dest_cell).style(theme::subtext()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(36),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Min(24),
        ],
    )
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

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    if let Some(ref toast) = app.toast {
        let style = if toast.is_error {
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
        frame.render_widget(Paragraph::new(format!(" {} ", toast.message)).style(style), chunks[0]);
    } else {
        let hint = match app.active_tab {
            ActiveTab::Search => {
                " [/] Search  [c] Cat  [S] Season  [i] Indexer  [o] Sort  [Enter/d] Download  [I] Import .nzb "
            }
            ActiveTab::Downloads => {
                " [Space] Pause Job  [P] Pause All  [x] Cancel  [c] Cleanup  [K/J] Move  [w] Switch Pane  [Enter] Details "
            }
            ActiveTab::Settings => {
                " [h/l] Switch Section  [a] Add  [e] Edit  [t] Test  [Space] Toggle  [d] Delete "
            }
            ActiveTab::Logs => " [j/k] Scroll  [G] Bottom  [a] Toggle Auto-Scroll ",
            ActiveTab::Export => {
                " [t] Cycle Tag  [e/Enter] Export Selected  [E] Export All Tagged  [m] Move/Copy "
            }
        };
        frame.render_widget(Paragraph::new(hint).style(theme::subtext()), chunks[0]);
    }

    let global_bar = Line::from(vec![
        Span::styled(" [1-5/Tab]", Style::default().fg(theme::ACCENT)),
        Span::styled(" Views  ", theme::muted()),
        Span::styled("[I]", Style::default().fg(theme::ACCENT)),
        Span::styled(" Import NZB  ", theme::muted()),
        Span::styled("[P]", Style::default().fg(theme::ACCENT)),
        Span::styled(" Global Pause  ", theme::muted()),
        Span::styled("[?]", Style::default().fg(theme::ACCENT)),
        Span::styled(" Help  ", theme::muted()),
        Span::styled("[q]", Style::default().fg(theme::ACCENT)),
        Span::styled(" Quit", theme::muted()),
    ]);
    frame.render_widget(Paragraph::new(global_bar), chunks[1]);
}

fn draw_provider_modal(frame: &mut Frame, form: &ProviderForm, area: Rect) {
    let popup = centered_rect(64, 17, area);
    frame.render_widget(Clear, popup);

    let title = if form.edit_index.is_some() {
        " Edit Usenet Provider "
    } else {
        " Add Usenet Provider "
    };

    let pw_display = if form.show_password {
        form.password.clone()
    } else {
        "•".repeat(form.password.chars().count())
    };

    let fields: Vec<(&str, String, bool)> = vec![
        ("Name", form.name.clone(), false),
        ("Host", form.host.clone(), false),
        ("Port", form.port.clone(), false),
        (
            "TLS (Space toggle)",
            if form.tls { "Yes (NNTPS)".into() } else { "No (Plain NNTP)".into() },
            true,
        ),
        ("Username", form.username.clone(), false),
        ("Password (Ctrl+R reveal)", pw_display, false),
        ("Connections (1–100)", form.connections.clone(), false),
        ("Priority (0=Primary, 1+=Backup)", form.priority.clone(), false),
        (
            "Enabled (Space toggle)",
            if form.enabled { "Yes".into() } else { "No".into() },
            true,
        ),
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (idx, (label, val, is_bool)) in fields.into_iter().enumerate() {
        let active = idx == form.field;
        let cursor = if active && !is_bool { "▌" } else { "" };
        let marker = if active { "▸ " } else { "  " };
        let label_style = if active { theme::title() } else { theme::subtext() };
        let val_style = if active {
            Style::default().fg(theme::TEXT).bg(theme::HIGHLIGHT_BG)
        } else {
            Style::default().fg(theme::TEXT)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{}{:<32} ", marker, label), label_style),
            Span::styled(format!(" {}{} ", val, cursor), val_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Tab/↑↓] Next/Prev Field   [Enter] Save   [Esc] Cancel",
        theme::muted(),
    )));

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .style(Style::default().bg(theme::MODAL_BG))
            .title(Span::styled(title, theme::title())),
    );
    frame.render_widget(widget, popup);
}

fn draw_indexer_modal(frame: &mut Frame, form: &IndexerForm, area: Rect) {
    let popup = centered_rect(68, 12, area);
    frame.render_widget(Clear, popup);

    let title = if form.edit_index.is_some() {
        " Edit Newznab Indexer "
    } else {
        " Add Newznab Indexer "
    };

    let key_display = if form.show_key {
        form.api_key.clone()
    } else {
        "•".repeat(form.api_key.chars().count())
    };

    let fields: Vec<(&str, String, bool)> = vec![
        ("Name", form.name.clone(), false),
        ("URL (e.g. https://api.nzbgeek.info)", form.url.clone(), false),
        ("API Key (Ctrl+R reveal)", key_display, false),
        (
            "Enabled (Space toggle)",
            if form.enabled { "Yes".into() } else { "No".into() },
            true,
        ),
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (idx, (label, val, is_bool)) in fields.into_iter().enumerate() {
        let active = idx == form.field;
        let cursor = if active && !is_bool { "▌" } else { "" };
        let marker = if active { "▸ " } else { "  " };
        let label_style = if active { theme::title() } else { theme::subtext() };
        let val_style = if active {
            Style::default().fg(theme::TEXT).bg(theme::HIGHLIGHT_BG)
        } else {
            Style::default().fg(theme::TEXT)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{}{:<36} ", marker, label), label_style),
            Span::styled(format!(" {}{} ", val, cursor), val_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Tab/↑↓] Next/Prev Field   [Enter] Save   [Esc] Cancel",
        theme::muted(),
    )));

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .style(Style::default().bg(theme::MODAL_BG))
            .title(Span::styled(title, theme::title())),
    );
    frame.render_widget(widget, popup);
}

fn draw_export_target_modal(frame: &mut Frame, form: &ExportTargetForm, area: Rect) {
    let popup = centered_rect(64, 10, area);
    frame.render_widget(Clear, popup);

    let title = if form.edit_index.is_some() {
        " Edit Export Target "
    } else {
        " Add Export Target "
    };

    let fields = [
        ("Tag Name (e.g. Movie, TV)", form.tag.clone()),
        ("Destination Directory", form.path.clone()),
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (idx, (label, val)) in fields.into_iter().enumerate() {
        let active = idx == form.field;
        let cursor = if active { "▌" } else { "" };
        let marker = if active { "▸ " } else { "  " };
        let label_style = if active { theme::title() } else { theme::subtext() };
        let val_style = if active {
            Style::default().fg(theme::TEXT).bg(theme::HIGHLIGHT_BG)
        } else {
            Style::default().fg(theme::TEXT)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{}{:<28} ", marker, label), label_style),
            Span::styled(format!(" {}{} ", val, cursor), val_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Tab/↑↓] Next/Prev Field   [Enter] Save   [Esc] Cancel",
        theme::muted(),
    )));

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .style(Style::default().bg(theme::MODAL_BG))
            .title(Span::styled(title, theme::title())),
    );
    frame.render_widget(widget, popup);
}

fn draw_text_prompt_modal(frame: &mut Frame, prompt: &TextPrompt, area: Rect) {
    let popup = centered_rect(66, 8, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(format!(" {}", prompt.hint), theme::subtext())),
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}▌ ", prompt.value),
            Style::default().fg(theme::TEXT).bg(theme::HIGHLIGHT_BG),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " [Enter] Confirm   [Ctrl+U] Clear   [Esc] Cancel",
            theme::muted(),
        )),
    ];

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .style(Style::default().bg(theme::MODAL_BG))
            .title(Span::styled(format!(" {} ", prompt.title), theme::title())),
    );
    frame.render_widget(widget, popup);
}

fn draw_job_details_modal(frame: &mut Frame, job: &JobSummary, area: Rect) {
    let popup = centered_rect(72, 16, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Title:     ", theme::subtext()),
            Span::styled(job.title.clone(), Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" Status:    ", theme::subtext()),
            Span::styled(job.status.label(), status_style(job.status)),
            Span::styled(format!("  ({})", job.stage_detail), theme::muted()),
        ]),
        Line::from(vec![
            Span::styled(" Progress:  ", theme::subtext()),
            Span::styled(
                format!(
                    "{} / {} ({:.1}%)",
                    format_bytes(job.downloaded_bytes),
                    format_bytes(job.total_bytes),
                    job.progress_ratio() * 100.0
                ),
                Style::default().fg(theme::ACCENT),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Segments:  ", theme::subtext()),
            Span::raw(format!(
                "{} / {} done ({} missing)",
                job.completed_segments, job.total_segments, job.missing_segments
            )),
        ]),
        Line::from(vec![
            Span::styled(" Files:     ", theme::subtext()),
            Span::raw(format!("{} / {}", job.files_done, job.files_total)),
        ]),
    ];

    if let Some(ref cat) = job.category {
        lines.push(Line::from(vec![
            Span::styled(" Category:  ", theme::subtext()),
            Span::raw(cat.clone()),
        ]));
    }
    if let Some(ref path) = job.output_path {
        lines.push(Line::from(vec![
            Span::styled(" Output:    ", theme::subtext()),
            Span::styled(path.display().to_string(), theme::success()),
        ]));
    }
    if let Some(ref err) = job.error {
        lines.push(Line::from(vec![
            Span::styled(" Error:     ", theme::subtext()),
            Span::styled(err.clone(), theme::danger()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Press [Esc] or [Enter] to close",
        theme::muted(),
    )));

    let widget = Paragraph::new(lines)
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

fn draw_help_modal(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 27, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(" Global Keys", theme::title())),
        Line::from("   1 / 2 / 3 / 4 / 5   Jump to Search / Downloads / Settings / Logs / Export"),
        Line::from("   Tab / Shift+Tab     Cycle views"),
        Line::from("   I                   Import a local .nzb file"),
        Line::from("   P                   Pause / resume all downloads"),
        Line::from("   ? / q / Ctrl+C      Help / Quit"),
        Line::from(""),
        Line::from(Span::styled(" Search View", theme::title())),
        Line::from("   / or s              Focus search bar (Enter to run, Esc to blur)"),
        Line::from("   c / C               Cycle Newznab category filter"),
        Line::from("   S                   Filter by season (00 = full season packs)"),
        Line::from("   i / o               Cycle indexer filter / sort order"),
        Line::from("   Enter or d          Fetch NZB and enqueue download"),
        Line::from(""),
        Line::from(Span::styled(" Downloads View", theme::title())),
        Line::from("   Space / p           Pause or resume selected job"),
        Line::from("   x / Delete          Cancel active job or remove history item"),
        Line::from("   K / J               Move queued job priority up / down"),
        Line::from("   w / Enter           Switch Queue/History | Details"),
        Line::from("   c / C               Cleanup failed & temp dirs | Clear all history"),
        Line::from(""),
        Line::from(Span::styled(" Settings & Export Views", theme::title())),
        Line::from("   h / l               Switch Providers / Indexers / General / Export Targets"),
        Line::from("   a / e / d / t       Add / Edit / Delete / Live Test Connection"),
        Line::from("   t / e / E / m       Export: Cycle Tag / Export / Export All / Move-Copy"),
    ];

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_focused())
            .style(Style::default().bg(theme::MODAL_BG))
            .title(Span::styled(" nzbtui Keybindings — [Esc] Close ", theme::title())),
    );
    frame.render_widget(widget, popup);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
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
