mod config;
mod engine;
mod newznab;
mod nntp;
mod nzb;
mod postproc;
mod ui;
mod updater;
mod yenc;

use anyhow::Result;
use clap::Parser;
use config::{expand_tilde, AppConfig};
use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use engine::{DownloadEngine, LogLevel};
use futures::StreamExt;
use newznab::NewznabClient;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use ui::App;

#[derive(Debug, Parser)]
#[command(
    name = "nzbtui",
    version,
    about = "Standalone Rust TUI Usenet client (Ratatui + NNTP/TLS + Newznab + yEnc + PAR2 + Extract)"
)]
struct Cli {
    /// Custom path to JSON configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Immediately enqueue one or more local .nzb files on startup
    #[arg(short, long = "import", value_name = "NZB_FILE")]
    import_nzbs: Vec<PathBuf>,

    /// Print default config file path and exit
    #[arg(long)]
    print_config_path: bool,

    /// Check GitHub releases for a newer version and self-update the binary
    #[arg(long)]
    update: bool,
}

fn main() -> Result<()> {
    let mut rt_builder = tokio::runtime::Builder::new_multi_thread();
    rt_builder.enable_all();

    // Keep thread count and stack allocations lean on 32-bit x86 (e.g., iSH on iOS)
    #[cfg(target_arch = "x86")]
    {
        rt_builder
            .worker_threads(2)
            .max_blocking_threads(8)
            .thread_stack_size(2 * 1024 * 1024);
    }

    let rt = rt_builder.build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let cli = Cli::parse();

    let config_path = cli
        .config
        .map(|p| expand_tilde(&p))
        .unwrap_or_else(AppConfig::default_config_path);

    if cli.print_config_path {
        println!("{}", config_path.display());
        return Ok(());
    }

    if cli.update {
        let outcome = updater::check_and_self_update(true, true).await?;
        println!("{}", outcome.message);
        return Ok(());
    }

    let cfg = AppConfig::load(&config_path)?;
    let tick_rate = Duration::from_millis(cfg.ui.tick_rate_ms.clamp(50, 1000));
    let config_arc = Arc::new(RwLock::new(cfg.clone()));

    let state_dir = AppConfig::default_state_dir();
    let engine = DownloadEngine::new(Arc::clone(&config_arc), state_dir);
    let newznab = Arc::new(NewznabClient::new()?);

    engine
        .log(
            LogLevel::Info,
            format!(
                "nzbtui v{} started (config: {})",
                env!("CARGO_PKG_VERSION"),
                config_path.display()
            ),
        )
        .await;

    // Enqueue any CLI-supplied local NZB files
    for nzb_path in &cli.import_nzbs {
        let expanded = expand_tilde(nzb_path);
        match engine.enqueue_local_file(&expanded).await {
            Ok((_, title)) => {
                engine
                    .log(LogLevel::Info, format!("CLI imported NZB: {}", title))
                    .await;
            }
            Err(e) => {
                engine
                    .log(
                        LogLevel::Error,
                        format!("Failed CLI import {}: {}", expanded.display(), e),
                    )
                    .await;
            }
        }
    }

    let initial_snapshot = engine.snapshot().await;
    let mut app = App::new(
        cfg,
        config_arc,
        config_path,
        Arc::clone(&engine),
        newznab,
        initial_snapshot,
    );

    if !cli.import_nzbs.is_empty() {
        app.active_tab = ui::ActiveTab::Downloads;
    }

    // Install panic hook so terminal is always restored even on unexpected panic
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let run_result = run_event_loop(&mut terminal, &mut app, tick_rate).await;

    // Graceful cleanup
    engine.shutdown().await;
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    run_result
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    tick_rate: Duration,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(tick_rate);

    loop {
        terminal.draw(|f| ui::render::draw(f, app))?;

        if app.should_quit {
            break;
        }

        tokio::select! {
            _ = ticker.tick() => {
                app.on_tick().await;
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        // Ignore release/repeat events if terminal reports them
                        if key.kind == crossterm::event::KeyEventKind::Press {
                            app.handle_key(key).await;
                        }
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
