pub mod render;
pub mod theme;

use crate::config::{expand_tilde, AppConfig, ExportTarget, IndexerConfig, ProviderConfig};
use crate::engine::{format_bytes, DownloadEngine, EngineSnapshot, JobStatus, JobSummary, LogLevel};
use crate::newznab::{NewznabClient, SearchResult, CATEGORIES};
use crate::nntp;
use crate::nzb::NzbDocument;
use crate::postproc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Search,
    Downloads,
    Settings,
    Logs,
    Export,
}

impl ActiveTab {
    pub fn index(self) -> usize {
        match self {
            ActiveTab::Search => 0,
            ActiveTab::Downloads => 1,
            ActiveTab::Settings => 2,
            ActiveTab::Logs => 3,
            ActiveTab::Export => 4,
        }
    }

    pub fn next(self) -> Self {
        match self {
            ActiveTab::Search => ActiveTab::Downloads,
            ActiveTab::Downloads => ActiveTab::Settings,
            ActiveTab::Settings => ActiveTab::Logs,
            ActiveTab::Logs => ActiveTab::Export,
            ActiveTab::Export => ActiveTab::Search,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ActiveTab::Search => ActiveTab::Export,
            ActiveTab::Downloads => ActiveTab::Search,
            ActiveTab::Settings => ActiveTab::Downloads,
            ActiveTab::Logs => ActiveTab::Settings,
            ActiveTab::Export => ActiveTab::Logs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    DateDesc,
    SizeDesc,
    GrabsDesc,
    TitleAsc,
}

impl SearchSort {
    pub fn next(self) -> Self {
        match self {
            SearchSort::DateDesc => SearchSort::SizeDesc,
            SearchSort::SizeDesc => SearchSort::GrabsDesc,
            SearchSort::GrabsDesc => SearchSort::TitleAsc,
            SearchSort::TitleAsc => SearchSort::DateDesc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SearchSort::DateDesc => "Age",
            SearchSort::SizeDesc => "Size",
            SearchSort::GrabsDesc => "Grabs",
            SearchSort::TitleAsc => "Title",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadsPane {
    Queue,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Providers,
    Indexers,
    General,
    Export,
}

impl SettingsSection {
    pub fn next(self) -> Self {
        match self {
            SettingsSection::Providers => SettingsSection::Indexers,
            SettingsSection::Indexers => SettingsSection::General,
            SettingsSection::General => SettingsSection::Export,
            SettingsSection::Export => SettingsSection::Providers,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            SettingsSection::Providers => SettingsSection::Export,
            SettingsSection::Indexers => SettingsSection::Providers,
            SettingsSection::General => SettingsSection::Indexers,
            SettingsSection::Export => SettingsSection::General,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderForm {
    pub edit_index: Option<usize>,
    pub field: usize,
    pub name: String,
    pub host: String,
    pub port: String,
    pub tls: bool,
    pub username: String,
    pub password: String,
    pub connections: String,
    pub priority: String,
    pub enabled: bool,
    pub show_password: bool,
}

impl ProviderForm {
    pub const FIELD_COUNT: usize = 9;

    pub fn from_config(cfg: &ProviderConfig, edit_index: Option<usize>) -> Self {
        Self {
            edit_index,
            field: 0,
            name: cfg.name.clone(),
            host: cfg.host.clone(),
            port: cfg.port.to_string(),
            tls: cfg.tls,
            username: cfg.username.clone(),
            password: cfg.password.clone(),
            connections: cfg.connections.to_string(),
            priority: cfg.priority.to_string(),
            enabled: cfg.enabled,
            show_password: false,
        }
    }

    pub fn into_config(&self) -> Result<ProviderConfig, String> {
        let host = self.host.trim().to_string();
        if host.is_empty() {
            return Err("Host cannot be empty".into());
        }
        let port: u16 = self
            .port
            .trim()
            .parse()
            .map_err(|_| "Port must be a number (1–65535)".to_string())?;
        if port == 0 {
            return Err("Port must be > 0".into());
        }
        let connections: usize = self
            .connections
            .trim()
            .parse()
            .map_err(|_| "Connections must be a positive integer".to_string())?;
        if connections == 0 || connections > 100 {
            return Err("Connections must be between 1 and 100".into());
        }
        let priority: u8 = self
            .priority
            .trim()
            .parse()
            .map_err(|_| "Priority must be 0–255 (0 = primary)".to_string())?;
        let name = if self.name.trim().is_empty() {
            host.clone()
        } else {
            self.name.trim().to_string()
        };

        Ok(ProviderConfig {
            name,
            host,
            port,
            tls: self.tls,
            username: self.username.trim().to_string(),
            password: self.password.clone(),
            connections,
            priority,
            enabled: self.enabled,
        })
    }
}

#[derive(Debug, Clone)]
pub struct IndexerForm {
    pub edit_index: Option<usize>,
    pub field: usize,
    pub name: String,
    pub url: String,
    pub api_key: String,
    pub enabled: bool,
    pub show_key: bool,
}

impl IndexerForm {
    pub const FIELD_COUNT: usize = 4;

    pub fn from_config(cfg: &IndexerConfig, edit_index: Option<usize>) -> Self {
        Self {
            edit_index,
            field: 0,
            name: cfg.name.clone(),
            url: cfg.url.clone(),
            api_key: cfg.api_key.clone(),
            enabled: cfg.enabled,
            show_key: false,
        }
    }

    pub fn into_config(&self) -> Result<IndexerConfig, String> {
        let url = self.url.trim().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("URL must start with http:// or https://".into());
        }
        let api_key = self.api_key.trim().to_string();
        if api_key.is_empty() {
            return Err("API key cannot be empty".into());
        }
        let name = if self.name.trim().is_empty() {
            url.clone()
        } else {
            self.name.trim().to_string()
        };
        Ok(IndexerConfig {
            name,
            url,
            api_key,
            enabled: self.enabled,
        })
    }
}

#[derive(Debug, Clone)]
pub enum TextPromptKind {
    ImportNzb,
    SearchSeason,
    DownloadDir,
    TempDir,
    MaxConcurrentJobs,
}

#[derive(Debug, Clone)]
pub struct TextPrompt {
    pub kind: TextPromptKind,
    pub title: String,
    pub hint: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct ExportTargetForm {
    pub edit_index: Option<usize>,
    pub field: usize,
    pub tag: String,
    pub path: String,
}

impl ExportTargetForm {
    pub const FIELD_COUNT: usize = 2;

    pub fn new_empty() -> Self {
        Self {
            edit_index: None,
            field: 0,
            tag: String::new(),
            path: String::from("~/"),
        }
    }

    pub fn from_config(cfg: &ExportTarget, edit_index: Option<usize>) -> Self {
        Self {
            edit_index,
            field: 0,
            tag: cfg.tag.clone(),
            path: cfg.path.display().to_string(),
        }
    }

    pub fn into_config(&self) -> Result<ExportTarget, String> {
        let tag = self.tag.trim().to_string();
        if tag.is_empty() {
            return Err("Tag name cannot be empty (e.g. Movie, TV, Music)".into());
        }
        let raw_path = self.path.trim();
        if raw_path.is_empty() {
            return Err("Destination path cannot be empty".into());
        }
        Ok(ExportTarget {
            tag,
            path: expand_tilde(&PathBuf::from(raw_path)),
        })
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    Help,
    Provider(ProviderForm),
    Indexer(IndexerForm),
    ExportTarget(ExportTargetForm),
    TextPrompt(TextPrompt),
    JobDetails(JobSummary),
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub is_error: bool,
    pub expires_at: Instant,
}

pub enum AsyncEvent {
    SearchCompleted {
        results: Vec<SearchResult>,
        errors: Vec<String>,
    },
    NzbEnqueued {
        guid: String,
        title: String,
    },
    NzbError(String),
    TestResult {
        label: String,
        ok: bool,
        detail: String,
    },
    LocalImportResult(Result<String, String>),
}

pub struct ToolStatus {
    pub par2: bool,
    pub unrar: bool,
    pub seven_zip: bool,
    pub unzip: bool,
    pub tar: bool,
}

impl ToolStatus {
    pub fn detect() -> Self {
        Self {
            par2: postproc::which_binary(&["par2"]).is_some(),
            unrar: postproc::which_binary(&["unrar", "unrar-free"]).is_some(),
            seven_zip: postproc::which_binary(&["7z", "7zz", "7za"]).is_some(),
            unzip: postproc::which_binary(&["unzip"]).is_some(),
            tar: postproc::which_binary(&["tar"]).is_some(),
        }
    }
}

pub struct App {
    pub should_quit: bool,
    pub active_tab: ActiveTab,
    pub config: AppConfig,
    pub config_arc: Arc<RwLock<AppConfig>>,
    pub config_path: PathBuf,
    pub engine: Arc<DownloadEngine>,
    pub newznab: Arc<NewznabClient>,
    pub snapshot: EngineSnapshot,
    pub tools: ToolStatus,

    // Search state
    pub search_query: String,
    pub search_input_focused: bool,
    pub search_category_idx: usize,
    pub search_season: String,
    /// 0 = All Enabled Indexers, 1..=N = specific indexer
    pub search_indexer_idx: usize,
    pub search_sort: SearchSort,
    pub search_loading: bool,
    pub search_results: Vec<SearchResult>,
    pub search_selected: usize,
    pub queued_guids: HashSet<String>,

    // Downloads state
    pub downloads_pane: DownloadsPane,
    pub queue_selected: usize,
    pub history_selected: usize,

    // Settings state
    pub settings_section: SettingsSection,
    pub provider_selected: usize,
    pub indexer_selected: usize,
    pub general_selected: usize,
    pub export_target_selected: usize,
    pub testing_status: Option<String>,

    // Logs state
    pub log_scroll: usize,
    pub log_auto_scroll: bool,

    // Export state
    pub export_selected: usize,
    pub export_move_mode: bool,

    // Overlays
    pub modal: Option<Modal>,
    pub toast: Option<Toast>,

    // Async event channel
    pub async_tx: mpsc::UnboundedSender<AsyncEvent>,
    pub async_rx: mpsc::UnboundedReceiver<AsyncEvent>,
}

impl App {
    pub fn new(
        config: AppConfig,
        config_arc: Arc<RwLock<AppConfig>>,
        config_path: PathBuf,
        engine: Arc<DownloadEngine>,
        newznab: Arc<NewznabClient>,
        initial_snapshot: EngineSnapshot,
    ) -> Self {
        let (async_tx, async_rx) = mpsc::unbounded_channel();

        // If nothing is configured yet, start on Settings so the user can add provider + indexer immediately
        let initial_tab = if config.providers.is_empty() && config.indexers.is_empty() {
            ActiveTab::Settings
        } else {
            ActiveTab::Search
        };

        let mut app = Self {
            should_quit: false,
            active_tab: initial_tab,
            config,
            config_arc,
            config_path,
            engine,
            newznab,
            snapshot: initial_snapshot,
            tools: ToolStatus::detect(),

            search_query: String::new(),
            search_input_focused: false,
            search_category_idx: 0,
            search_season: String::new(),
            search_indexer_idx: 0,
            search_sort: SearchSort::DateDesc,
            search_loading: false,
            search_results: Vec::new(),
            search_selected: 0,
            queued_guids: HashSet::new(),

            downloads_pane: DownloadsPane::Queue,
            queue_selected: 0,
            history_selected: 0,

            settings_section: SettingsSection::Providers,
            provider_selected: 0,
            indexer_selected: 0,
            general_selected: 0,
            export_target_selected: 0,
            testing_status: None,

            log_scroll: 0,
            log_auto_scroll: true,

            export_selected: 0,
            export_move_mode: true,

            modal: None,
            toast: None,

            async_tx,
            async_rx,
        };

        if app.config.providers.is_empty() && app.config.indexers.is_empty() {
            app.set_toast(
                "Welcome to nzbtui! Press [a] to add a Usenet provider, [l] -> [a] for an indexer.",
                false,
            );
        }
        app
    }

    pub fn set_toast(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toast = Some(Toast {
            message: msg.into(),
            is_error,
            expires_at: Instant::now() + Duration::from_secs(6),
        });
    }

    pub async fn on_tick(&mut self) {
        self.snapshot = self.engine.snapshot().await;

        // Clamp selections
        if !self.snapshot.queue.is_empty() {
            self.queue_selected = self.queue_selected.min(self.snapshot.queue.len() - 1);
        } else {
            self.queue_selected = 0;
        }
        if !self.snapshot.history.is_empty() {
            self.history_selected = self.history_selected.min(self.snapshot.history.len() - 1);
        } else {
            self.history_selected = 0;
        }

        if let Some(ref t) = self.toast {
            if Instant::now() >= t.expires_at {
                self.toast = None;
            }
        }

        // Drain async events
        while let Ok(ev) = self.async_rx.try_recv() {
            match ev {
                AsyncEvent::SearchCompleted { results, errors } => {
                    self.search_loading = false;
                    self.search_results = results;
                    self.apply_search_sort();
                    self.search_selected = 0;
                    if !errors.is_empty() {
                        self.set_toast(
                            format!(
                                "Found {} results ({} error: {})",
                                self.search_results.len(),
                                errors.len(),
                                errors[0]
                            ),
                            true,
                        );
                    } else {
                        self.set_toast(
                            format!("Found {} results", self.search_results.len()),
                            false,
                        );
                    }
                }
                AsyncEvent::NzbEnqueued { guid, title } => {
                    self.queued_guids.insert(guid);
                    self.set_toast(format!("Queued: {}", title), false);
                }
                AsyncEvent::NzbError(err) => {
                    self.set_toast(err, true);
                }
                AsyncEvent::TestResult { label, ok, detail } => {
                    self.testing_status = None;
                    let msg = format!("{}: {}", label, detail);
                    self.set_toast(msg.clone(), !ok);
                    self.engine
                        .log(
                            if ok {
                                LogLevel::Success
                            } else {
                                LogLevel::Error
                            },
                            msg,
                        )
                        .await;
                }
                AsyncEvent::LocalImportResult(res) => match res {
                    Ok(title) => self.set_toast(format!("Imported NZB: {}", title), false),
                    Err(e) => self.set_toast(format!("Import failed: {}", e), true),
                },
            }
        }
    }

    pub async fn handle_key(&mut self, key: KeyEvent) {
        // Global Ctrl+C quit
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // Modal captures all input when open
        if self.modal.is_some() {
            self.handle_modal_key(key).await;
            return;
        }

        // Search input box captures text when focused
        if self.active_tab == ActiveTab::Search && self.search_input_focused {
            match key.code {
                KeyCode::Esc => {
                    self.search_input_focused = false;
                }
                KeyCode::Enter => {
                    self.search_input_focused = false;
                    self.trigger_search();
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search_query.clear();
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                }
                _ => {}
            }
            return;
        }

        // Global navigation keys
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                return;
            }
            KeyCode::Char('1') => {
                self.active_tab = ActiveTab::Search;
                return;
            }
            KeyCode::Char('2') => {
                self.active_tab = ActiveTab::Downloads;
                return;
            }
            KeyCode::Char('3') => {
                self.active_tab = ActiveTab::Settings;
                return;
            }
            KeyCode::Char('4') => {
                self.active_tab = ActiveTab::Logs;
                return;
            }
            KeyCode::Char('5') => {
                self.active_tab = ActiveTab::Export;
                return;
            }
            KeyCode::Tab => {
                self.active_tab = self.active_tab.next();
                return;
            }
            KeyCode::BackTab => {
                self.active_tab = self.active_tab.prev();
                return;
            }
            KeyCode::Char('I') => {
                self.modal = Some(Modal::TextPrompt(TextPrompt {
                    kind: TextPromptKind::ImportNzb,
                    title: "Import Local .nzb File".into(),
                    hint: "Enter full or ~/ path to an .nzb file and press Enter".into(),
                    value: String::from("~/Downloads/"),
                }));
                return;
            }
            KeyCode::Char('P') => {
                let paused = self.engine.toggle_global_pause().await;
                self.set_toast(
                    if paused {
                        "All downloads paused"
                    } else {
                        "Downloads resumed"
                    },
                    false,
                );
                return;
            }
            _ => {}
        }

        match self.active_tab {
            ActiveTab::Search => self.handle_search_key(key),
            ActiveTab::Downloads => self.handle_downloads_key(key).await,
            ActiveTab::Settings => self.handle_settings_key(key).await,
            ActiveTab::Logs => self.handle_logs_key(key),
            ActiveTab::Export => self.handle_export_key(key).await,
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('/') | KeyCode::Char('s') => {
                self.search_input_focused = true;
            }
            KeyCode::Char('c') => {
                self.search_category_idx = (self.search_category_idx + 1) % CATEGORIES.len();
            }
            KeyCode::Char('C') => {
                self.search_category_idx = if self.search_category_idx == 0 {
                    CATEGORIES.len() - 1
                } else {
                    self.search_category_idx - 1
                };
            }
            KeyCode::Char('S') => {
                self.modal = Some(Modal::TextPrompt(TextPrompt {
                    kind: TextPromptKind::SearchSeason,
                    title: "Filter by Season".into(),
                    hint: "Season number (00 = full season/series packs on NZBGeek, empty = All)"
                        .into(),
                    value: self.search_season.clone(),
                }));
            }
            KeyCode::Char('i') => {
                let count = self.config.enabled_indexers().len() + 1;
                self.search_indexer_idx = (self.search_indexer_idx + 1) % count.max(1);
            }
            KeyCode::Char('o') => {
                self.search_sort = self.search_sort.next();
                self.apply_search_sort();
            }
            KeyCode::Char('r') => {
                self.trigger_search();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.search_results.is_empty() {
                    self.search_selected =
                        (self.search_selected + 1).min(self.search_results.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.search_selected = self.search_selected.saturating_sub(1);
            }
            KeyCode::PageDown => {
                if !self.search_results.is_empty() {
                    self.search_selected =
                        (self.search_selected + 10).min(self.search_results.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.search_selected = self.search_selected.saturating_sub(10);
            }
            KeyCode::Enter | KeyCode::Char('d') => {
                self.download_selected_search_result();
            }
            _ => {}
        }
    }

    async fn handle_downloads_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('w') | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                self.downloads_pane = match self.downloads_pane {
                    DownloadsPane::Queue => DownloadsPane::History,
                    DownloadsPane::History => DownloadsPane::Queue,
                };
            }
            KeyCode::Down | KeyCode::Char('j') => match self.downloads_pane {
                DownloadsPane::Queue => {
                    if !self.snapshot.queue.is_empty() {
                        self.queue_selected =
                            (self.queue_selected + 1).min(self.snapshot.queue.len() - 1);
                    }
                }
                DownloadsPane::History => {
                    if !self.snapshot.history.is_empty() {
                        self.history_selected =
                            (self.history_selected + 1).min(self.snapshot.history.len() - 1);
                    }
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match self.downloads_pane {
                DownloadsPane::Queue => {
                    self.queue_selected = self.queue_selected.saturating_sub(1);
                }
                DownloadsPane::History => {
                    self.history_selected = self.history_selected.saturating_sub(1);
                }
            },
            KeyCode::Char(' ') | KeyCode::Char('p') => {
                if self.downloads_pane == DownloadsPane::Queue {
                    if let Some(job) = self.snapshot.queue.get(self.queue_selected) {
                        self.engine.toggle_job_pause(job.id).await;
                    }
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => match self.downloads_pane {
                DownloadsPane::Queue => {
                    if let Some(job) = self.snapshot.queue.get(self.queue_selected) {
                        let title = job.title.clone();
                        self.engine.cancel_job(job.id).await;
                        self.set_toast(format!("Cancelling: {}", title), false);
                    }
                }
                DownloadsPane::History => {
                    if let Some(job) = self.snapshot.history.get(self.history_selected) {
                        self.engine.remove_history_item(job.id).await;
                    }
                }
            },
            KeyCode::Char('K') => {
                if self.downloads_pane == DownloadsPane::Queue {
                    if let Some(job) = self.snapshot.queue.get(self.queue_selected) {
                        self.engine.move_job(job.id, true).await;
                        self.queue_selected = self.queue_selected.saturating_sub(1);
                    }
                }
            }
            KeyCode::Char('J') => {
                if self.downloads_pane == DownloadsPane::Queue {
                    if let Some(job) = self.snapshot.queue.get(self.queue_selected) {
                        self.engine.move_job(job.id, false).await;
                        if self.queue_selected + 1 < self.snapshot.queue.len() {
                            self.queue_selected += 1;
                        }
                    }
                }
            }
            KeyCode::Char('c') => match self.engine.cleanup_downloads().await {
                Ok(report) => {
                    self.snapshot = self.engine.snapshot().await;
                    if report.removed_history == 0 && report.removed_temp_dirs == 0 {
                        self.set_toast("Nothing to clean up", false);
                    } else {
                        self.set_toast(
                            format!(
                                "Cleanup: {} failed/cancelled removed, {} temp dir(s) ({} freed)",
                                report.removed_history,
                                report.removed_temp_dirs,
                                format_bytes(report.freed_bytes)
                            ),
                            false,
                        );
                    }
                }
                Err(err) => {
                    self.set_toast(format!("Cleanup failed: {}", err), true);
                }
            },
            KeyCode::Char('C') => {
                self.engine.clear_history().await;
                self.set_toast("Cleared download history", false);
            }
            KeyCode::Enter => {
                let job = match self.downloads_pane {
                    DownloadsPane::Queue => self.snapshot.queue.get(self.queue_selected).cloned(),
                    DownloadsPane::History => {
                        self.snapshot.history.get(self.history_selected).cloned()
                    }
                };
                if let Some(j) = job {
                    self.modal = Some(Modal::JobDetails(j));
                }
            }
            _ => {}
        }
    }

    async fn handle_settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Right | KeyCode::Char('l') => {
                self.settings_section = self.settings_section.next();
                return;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.settings_section = self.settings_section.prev();
                return;
            }
            _ => {}
        }

        match self.settings_section {
            SettingsSection::Providers => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    if !self.config.providers.is_empty() {
                        self.provider_selected =
                            (self.provider_selected + 1).min(self.config.providers.len() - 1);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.provider_selected = self.provider_selected.saturating_sub(1);
                }
                KeyCode::Char('a') => {
                    let tmpl = ProviderConfig::new_template();
                    self.modal = Some(Modal::Provider(ProviderForm::from_config(&tmpl, None)));
                }
                KeyCode::Char('e') | KeyCode::Enter => {
                    if let Some(p) = self.config.providers.get(self.provider_selected) {
                        self.modal = Some(Modal::Provider(ProviderForm::from_config(
                            p,
                            Some(self.provider_selected),
                        )));
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(p) = self.config.providers.get_mut(self.provider_selected) {
                        p.enabled = !p.enabled;
                        self.persist_config().await;
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if self.provider_selected < self.config.providers.len() {
                        let removed = self.config.providers.remove(self.provider_selected);
                        if self.provider_selected > 0
                            && self.provider_selected >= self.config.providers.len()
                        {
                            self.provider_selected -= 1;
                        }
                        self.persist_config().await;
                        self.set_toast(format!("Removed provider '{}'", removed.name), false);
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(p) = self.config.providers.get(self.provider_selected).cloned() {
                        self.testing_status = Some(format!("Testing NNTP '{}'…", p.name));
                        let tx = self.async_tx.clone();
                        tokio::spawn(async move {
                            let label = format!("Provider '{}'", p.name);
                            match nntp::test_provider(&p).await {
                                Ok(detail) => {
                                    let _ = tx.send(AsyncEvent::TestResult {
                                        label,
                                        ok: true,
                                        detail,
                                    });
                                }
                                Err(detail) => {
                                    let _ = tx.send(AsyncEvent::TestResult {
                                        label,
                                        ok: false,
                                        detail,
                                    });
                                }
                            }
                        });
                    }
                }
                _ => {}
            },
            SettingsSection::Indexers => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    if !self.config.indexers.is_empty() {
                        self.indexer_selected =
                            (self.indexer_selected + 1).min(self.config.indexers.len() - 1);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.indexer_selected = self.indexer_selected.saturating_sub(1);
                }
                KeyCode::Char('a') => {
                    let tmpl = IndexerConfig::new_template();
                    self.modal = Some(Modal::Indexer(IndexerForm::from_config(&tmpl, None)));
                }
                KeyCode::Char('e') | KeyCode::Enter => {
                    if let Some(idx) = self.config.indexers.get(self.indexer_selected) {
                        self.modal = Some(Modal::Indexer(IndexerForm::from_config(
                            idx,
                            Some(self.indexer_selected),
                        )));
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(idx) = self.config.indexers.get_mut(self.indexer_selected) {
                        idx.enabled = !idx.enabled;
                        self.persist_config().await;
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if self.indexer_selected < self.config.indexers.len() {
                        let removed = self.config.indexers.remove(self.indexer_selected);
                        if self.indexer_selected > 0
                            && self.indexer_selected >= self.config.indexers.len()
                        {
                            self.indexer_selected -= 1;
                        }
                        self.search_indexer_idx = 0;
                        self.persist_config().await;
                        self.set_toast(format!("Removed indexer '{}'", removed.name), false);
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(idx) = self.config.indexers.get(self.indexer_selected).cloned() {
                        self.testing_status = Some(format!("Testing indexer '{}'…", idx.name));
                        let tx = self.async_tx.clone();
                        let client = Arc::clone(&self.newznab);
                        tokio::spawn(async move {
                            let label = format!("Indexer '{}'", idx.name);
                            match client.test_indexer(&idx).await {
                                Ok(detail) => {
                                    let _ = tx.send(AsyncEvent::TestResult {
                                        label,
                                        ok: true,
                                        detail,
                                    });
                                }
                                Err(detail) => {
                                    let _ = tx.send(AsyncEvent::TestResult {
                                        label,
                                        ok: false,
                                        detail,
                                    });
                                }
                            }
                        });
                    }
                }
                _ => {}
            },
            SettingsSection::General => {
                const GENERAL_ITEMS: usize = 8;
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.general_selected = (self.general_selected + 1).min(GENERAL_ITEMS - 1);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.general_selected = self.general_selected.saturating_sub(1);
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => match self.general_selected {
                        0 => {
                            self.modal = Some(Modal::TextPrompt(TextPrompt {
                                kind: TextPromptKind::DownloadDir,
                                title: "Download Directory".into(),
                                hint: "Completed files will be placed here".into(),
                                value: self.config.downloads.download_dir.display().to_string(),
                            }));
                        }
                        1 => {
                            self.modal = Some(Modal::TextPrompt(TextPrompt {
                                kind: TextPromptKind::TempDir,
                                title: "Incomplete / Temp Directory".into(),
                                hint: "In-progress segments & PAR2 work happen here".into(),
                                value: self.config.downloads.temp_dir.display().to_string(),
                            }));
                        }
                        2 => {
                            self.modal = Some(Modal::TextPrompt(TextPrompt {
                                kind: TextPromptKind::MaxConcurrentJobs,
                                title: "Max Concurrent Jobs".into(),
                                hint: "Number of NZBs downloading simultaneously (1-8)".into(),
                                value: self.config.downloads.max_concurrent_jobs.to_string(),
                            }));
                        }
                        3 => {
                            self.config.downloads.post_process.verify_par2 =
                                !self.config.downloads.post_process.verify_par2;
                            self.persist_config().await;
                        }
                        4 => {
                            self.config.downloads.post_process.repair_par2 =
                                !self.config.downloads.post_process.repair_par2;
                            self.persist_config().await;
                        }
                        5 => {
                            self.config.downloads.post_process.extract_archives =
                                !self.config.downloads.post_process.extract_archives;
                            self.persist_config().await;
                        }
                        6 => {
                            self.config.downloads.post_process.cleanup_after_extract =
                                !self.config.downloads.post_process.cleanup_after_extract;
                            self.persist_config().await;
                        }
                        7 => {
                            self.config.downloads.post_process.create_job_subfolder =
                                !self.config.downloads.post_process.create_job_subfolder;
                            self.persist_config().await;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            SettingsSection::Export => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    if !self.config.export_targets.is_empty() {
                        self.export_target_selected = (self.export_target_selected + 1)
                            .min(self.config.export_targets.len() - 1);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.export_target_selected = self.export_target_selected.saturating_sub(1);
                }
                KeyCode::Char('a') => {
                    self.modal = Some(Modal::ExportTarget(ExportTargetForm::new_empty()));
                }
                KeyCode::Char('e') | KeyCode::Enter => {
                    if let Some(target) = self.config.export_targets.get(self.export_target_selected)
                    {
                        self.modal = Some(Modal::ExportTarget(ExportTargetForm::from_config(
                            target,
                            Some(self.export_target_selected),
                        )));
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if self.export_target_selected < self.config.export_targets.len() {
                        let removed = self.config.export_targets.remove(self.export_target_selected);
                        self.export_target_selected = self
                            .export_target_selected
                            .min(self.config.export_targets.len().saturating_sub(1));
                        self.persist_config().await;
                        self.set_toast(format!("Deleted export target '{}'", removed.tag), false);
                    }
                }
                _ => {}
            },
        }
    }

    fn handle_logs_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.log_auto_scroll = false;
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.log_scroll = self.log_scroll.saturating_add(1);
            }
            KeyCode::Char('G') => {
                self.log_auto_scroll = true;
            }
            KeyCode::Char('a') => {
                self.log_auto_scroll = !self.log_auto_scroll;
            }
            _ => {}
        }
    }

    pub fn export_items(&self) -> Vec<&JobSummary> {
        self.snapshot
            .history
            .iter()
            .filter(|j| j.status == JobStatus::Completed)
            .collect()
    }

    async fn handle_export_key(&mut self, key: KeyEvent) {
        let items_len = self.export_items().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if items_len > 0 {
                    self.export_selected = (self.export_selected + 1).min(items_len - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.export_selected = self.export_selected.saturating_sub(1);
            }
            KeyCode::Char('m') => {
                self.export_move_mode = !self.export_move_mode;
                self.set_toast(
                    if self.export_move_mode {
                        "Export mode: MOVE files"
                    } else {
                        "Export mode: COPY files"
                    },
                    false,
                );
            }
            KeyCode::Char('t') => {
                if self.config.export_targets.is_empty() {
                    self.set_toast(
                        "No export targets configured — press [3] Settings -> Export Targets -> [a]",
                        true,
                    );
                    return;
                }
                let Some(job) = self.export_items().get(self.export_selected).cloned().cloned()
                else {
                    return;
                };
                let next_tag = match job.export_tag.as_deref() {
                    None => Some(self.config.export_targets[0].tag.clone()),
                    Some(current) => {
                        let idx = self
                            .config
                            .export_targets
                            .iter()
                            .position(|t| t.tag.eq_ignore_ascii_case(current));
                        match idx {
                            Some(i) if i + 1 < self.config.export_targets.len() => {
                                Some(self.config.export_targets[i + 1].tag.clone())
                            }
                            _ => None,
                        }
                    }
                };
                self.engine
                    .set_history_export_tag(job.id, next_tag.clone())
                    .await;
                self.snapshot = self.engine.snapshot().await;
                let label = next_tag.unwrap_or_else(|| "(untagged)".to_string());
                self.set_toast(format!("Tagged '{}' as {}", job.title, label), false);
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                let Some(job) = self.export_items().get(self.export_selected).cloned().cloned()
                else {
                    return;
                };
                let Some(tag) = job.export_tag.as_deref() else {
                    self.set_toast("Item has no tag — press [t] to tag it first", true);
                    return;
                };
                let Some(dest_dir) = self.config.export_path_for_tag(tag).cloned() else {
                    self.set_toast(
                        format!("No export path configured for tag '{}' in Settings", tag),
                        true,
                    );
                    return;
                };
                match self
                    .engine
                    .export_history_item(job.id, &dest_dir, self.export_move_mode)
                    .await
                {
                    Ok(dest) => {
                        self.snapshot = self.engine.snapshot().await;
                        self.set_toast(format!("Exported to {}", dest.display()), false);
                    }
                    Err(err) => {
                        self.set_toast(format!("Export failed: {}", err), true);
                    }
                }
            }
            KeyCode::Char('E') => {
                let candidates: Vec<(uuid::Uuid, PathBuf)> = self
                    .export_items()
                    .into_iter()
                    .filter(|j| j.exported_to.is_none())
                    .filter_map(|j| {
                        let tag = j.export_tag.as_deref()?;
                        let dest = self.config.export_path_for_tag(tag)?.clone();
                        Some((j.id, dest))
                    })
                    .collect();

                if candidates.is_empty() {
                    self.set_toast("No untagged or pending items to export", false);
                    return;
                }

                let mut ok_count = 0usize;
                let mut err_count = 0usize;
                for (id, dest_dir) in candidates {
                    match self
                        .engine
                        .export_history_item(id, &dest_dir, self.export_move_mode)
                        .await
                    {
                        Ok(_) => ok_count += 1,
                        Err(_) => err_count += 1,
                    }
                }
                self.snapshot = self.engine.snapshot().await;
                self.set_toast(
                    format!("Batch export: {} succeeded, {} failed", ok_count, err_count),
                    err_count > 0,
                );
            }
            _ => {}
        }
    }

    async fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };

        match modal {
            Modal::Help | Modal::JobDetails(_) => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')) {
                    self.modal = Some(modal);
                }
            }
            Modal::Provider(mut form) => {
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % ProviderForm::FIELD_COUNT;
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = if form.field == 0 {
                            ProviderForm::FIELD_COUNT - 1
                        } else {
                            form.field - 1
                        };
                    }
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        form.show_password = !form.show_password;
                    }
                    KeyCode::Enter => match form.into_config() {
                        Ok(cfg) => {
                            let name = cfg.name.clone();
                            if let Some(idx) = form.edit_index {
                                if idx < self.config.providers.len() {
                                    self.config.providers[idx] = cfg;
                                }
                            } else {
                                self.config.providers.push(cfg);
                                self.provider_selected = self.config.providers.len() - 1;
                            }
                            self.persist_config().await;
                            self.set_toast(format!("Saved provider '{}'", name), false);
                            return;
                        }
                        Err(err) => {
                            self.set_toast(err, true);
                        }
                    },
                    KeyCode::Char(' ') if form.field == 3 => {
                        form.tls = !form.tls;
                        if form.tls && form.port == "119" {
                            form.port = "563".into();
                        } else if !form.tls && form.port == "563" {
                            form.port = "119".into();
                        }
                    }
                    KeyCode::Char(' ') if form.field == 8 => {
                        form.enabled = !form.enabled;
                    }
                    KeyCode::Backspace => {
                        if let Some(s) = provider_field_mut(&mut form) {
                            s.pop();
                        }
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(s) = provider_field_mut(&mut form) {
                            s.clear();
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(s) = provider_field_mut(&mut form) {
                            s.push(c);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Provider(form));
            }
            Modal::Indexer(mut form) => {
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % IndexerForm::FIELD_COUNT;
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = if form.field == 0 {
                            IndexerForm::FIELD_COUNT - 1
                        } else {
                            form.field - 1
                        };
                    }
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        form.show_key = !form.show_key;
                    }
                    KeyCode::Enter => match form.into_config() {
                        Ok(cfg) => {
                            let name = cfg.name.clone();
                            if let Some(idx) = form.edit_index {
                                if idx < self.config.indexers.len() {
                                    self.config.indexers[idx] = cfg;
                                }
                            } else {
                                self.config.indexers.push(cfg);
                                self.indexer_selected = self.config.indexers.len() - 1;
                            }
                            self.persist_config().await;
                            self.set_toast(format!("Saved indexer '{}'", name), false);
                            return;
                        }
                        Err(err) => {
                            self.set_toast(err, true);
                        }
                    },
                    KeyCode::Char(' ') if form.field == 3 => {
                        form.enabled = !form.enabled;
                    }
                    KeyCode::Backspace => {
                        if let Some(s) = indexer_field_mut(&mut form) {
                            s.pop();
                        }
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(s) = indexer_field_mut(&mut form) {
                            s.clear();
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(s) = indexer_field_mut(&mut form) {
                            s.push(c);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Indexer(form));
            }
            Modal::ExportTarget(mut form) => {
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % ExportTargetForm::FIELD_COUNT;
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = if form.field == 0 {
                            ExportTargetForm::FIELD_COUNT - 1
                        } else {
                            form.field - 1
                        };
                    }
                    KeyCode::Enter => match form.into_config() {
                        Ok(cfg) => {
                            let tag = cfg.tag.clone();
                            if let Some(idx) = form.edit_index {
                                if idx < self.config.export_targets.len() {
                                    self.config.export_targets[idx] = cfg;
                                }
                            } else {
                                self.config.export_targets.push(cfg);
                                self.export_target_selected =
                                    self.config.export_targets.len() - 1;
                            }
                            self.persist_config().await;
                            self.set_toast(format!("Saved export target '{}'", tag), false);
                            return;
                        }
                        Err(err) => {
                            self.set_toast(err, true);
                        }
                    },
                    KeyCode::Backspace => {
                        if let Some(s) = export_target_field_mut(&mut form) {
                            s.pop();
                        }
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(s) = export_target_field_mut(&mut form) {
                            s.clear();
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(s) = export_target_field_mut(&mut form) {
                            s.push(c);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::ExportTarget(form));
            }
            Modal::TextPrompt(mut prompt) => {
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Backspace => {
                        prompt.value.pop();
                        self.modal = Some(Modal::TextPrompt(prompt));
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        prompt.value.clear();
                        self.modal = Some(Modal::TextPrompt(prompt));
                    }
                    KeyCode::Char(c) => {
                        prompt.value.push(c);
                        self.modal = Some(Modal::TextPrompt(prompt));
                    }
                    KeyCode::Enter => {
                        let val = prompt.value.trim().to_string();
                        match prompt.kind {
                            TextPromptKind::ImportNzb => {
                                let path = expand_tilde(&PathBuf::from(&val));
                                let engine = Arc::clone(&self.engine);
                                let tx = self.async_tx.clone();
                                tokio::spawn(async move {
                                    let res = engine
                                        .enqueue_local_file(&path)
                                        .await
                                        .map(|(_, title)| title)
                                        .map_err(|e| e.to_string());
                                    let _ = tx.send(AsyncEvent::LocalImportResult(res));
                                });
                            }
                            TextPromptKind::SearchSeason => {
                                self.search_season = val;
                                if !self.config.enabled_indexers().is_empty() {
                                    self.trigger_search();
                                } else if self.search_season.is_empty() {
                                    self.set_toast("Season filter cleared (All)", false);
                                } else {
                                    self.set_toast(
                                        format!("Season filter set to '{}'", self.search_season),
                                        false,
                                    );
                                }
                            }
                            TextPromptKind::DownloadDir => {
                                if !val.is_empty() {
                                    self.config.downloads.download_dir =
                                        expand_tilde(&PathBuf::from(val));
                                    self.persist_config().await;
                                }
                            }
                            TextPromptKind::TempDir => {
                                if !val.is_empty() {
                                    self.config.downloads.temp_dir =
                                        expand_tilde(&PathBuf::from(val));
                                    self.persist_config().await;
                                }
                            }
                            TextPromptKind::MaxConcurrentJobs => {
                                if let Ok(n) = val.parse::<usize>() {
                                    if (1..=8).contains(&n) {
                                        self.config.downloads.max_concurrent_jobs = n;
                                        self.persist_config().await;
                                    } else {
                                        self.set_toast("Must be between 1 and 8", true);
                                    }
                                } else {
                                    self.set_toast("Invalid number", true);
                                }
                            }
                        }
                    }
                    _ => {
                        self.modal = Some(Modal::TextPrompt(prompt));
                    }
                }
            }
        }
    }

    async fn persist_config(&mut self) {
        {
            let mut guard = self.config_arc.write().await;
            *guard = self.config.clone();
        }
        if let Err(e) = self.config.save(&self.config_path) {
            self.set_toast(format!("Failed saving config: {}", e), true);
        }
    }

    fn trigger_search(&mut self) {
        let enabled = self.config.enabled_indexers();
        if enabled.is_empty() {
            self.set_toast(
                "No enabled indexers configured — press [3] Settings -> Indexers -> [a]",
                true,
            );
            return;
        }
        let target_indexers = if self.search_indexer_idx == 0 {
            enabled
        } else if let Some(idx) = enabled.get(self.search_indexer_idx - 1) {
            vec![idx.clone()]
        } else {
            enabled
        };

        let query = self.search_query.clone();
        let cat = CATEGORIES[self.search_category_idx].id.to_string();
        let season = self.search_season.trim().to_string();
        let limit = self.config.ui.search_limit;
        let client = Arc::clone(&self.newznab);
        let tx = self.async_tx.clone();
        let engine = Arc::clone(&self.engine);

        self.search_loading = true;
        tokio::spawn(async move {
            let season_opt = if season.is_empty() {
                None
            } else {
                Some(season.as_str())
            };
            engine
                .log(
                    LogLevel::Info,
                    format!(
                        "Searching {} indexer(s) for '{}' (cat={}, season={})",
                        target_indexers.len(),
                        if query.is_empty() { "<latest>" } else { &query },
                        if cat.is_empty() { "all" } else { &cat },
                        season_opt.unwrap_or("all")
                    ),
                )
                .await;
            let (results, errors) = client
                .search_all(&target_indexers, &query, &cat, season_opt, limit)
                .await;
            let _ = tx.send(AsyncEvent::SearchCompleted { results, errors });
        });
    }

    fn download_selected_search_result(&mut self) {
        let Some(item) = self.search_results.get(self.search_selected).cloned() else {
            return;
        };
        if self.config.enabled_providers_sorted().is_empty() {
            self.set_toast(
                "Configure a Usenet provider first ([3] Settings -> Providers -> [a])",
                true,
            );
            return;
        }

        let client = Arc::clone(&self.newznab);
        let engine = Arc::clone(&self.engine);
        let tx = self.async_tx.clone();
        self.set_toast(format!("Fetching NZB: {}…", item.title), false);

        tokio::spawn(async move {
            match client.fetch_nzb(&item.nzb_url, &item.indexer_api_key).await {
                Ok(bytes) => match NzbDocument::parse(&bytes, &item.title) {
                    Ok(mut nzb) => {
                        if nzb.category.is_none() && !item.category.is_empty() {
                            nzb.category = Some(item.category.clone());
                        }
                        let title = nzb.title.clone();
                        engine.enqueue_nzb(nzb).await;
                        let _ = tx.send(AsyncEvent::NzbEnqueued {
                            guid: item.guid,
                            title,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AsyncEvent::NzbError(format!("NZB parse error: {}", e)));
                    }
                },
                Err(e) => {
                    let _ = tx.send(AsyncEvent::NzbError(format!("NZB fetch failed: {}", e)));
                }
            }
        });
    }

    fn apply_search_sort(&mut self) {
        match self.search_sort {
            SearchSort::DateDesc => self.search_results.sort_by(|a, b| b.pub_date.cmp(&a.pub_date)),
            SearchSort::SizeDesc => self.search_results.sort_by(|a, b| b.size.cmp(&a.size)),
            SearchSort::GrabsDesc => self.search_results.sort_by(|a, b| b.grabs.cmp(&a.grabs)),
            SearchSort::TitleAsc => self
                .search_results
                .sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        }
    }

    pub fn current_indexer_filter_label(&self) -> String {
        let enabled = self.config.enabled_indexers();
        if self.search_indexer_idx == 0 {
            format!("All ({})", enabled.len())
        } else {
            enabled
                .get(self.search_indexer_idx - 1)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| "All".to_string())
        }
    }
}

fn provider_field_mut(form: &mut ProviderForm) -> Option<&mut String> {
    match form.field {
        0 => Some(&mut form.name),
        1 => Some(&mut form.host),
        2 => Some(&mut form.port),
        4 => Some(&mut form.username),
        5 => Some(&mut form.password),
        6 => Some(&mut form.connections),
        7 => Some(&mut form.priority),
        _ => None,
    }
}

fn indexer_field_mut(form: &mut IndexerForm) -> Option<&mut String> {
    match form.field {
        0 => Some(&mut form.name),
        1 => Some(&mut form.url),
        2 => Some(&mut form.api_key),
        _ => None,
    }
}

fn export_target_field_mut(form: &mut ExportTargetForm) -> Option<&mut String> {
    match form.field {
        0 => Some(&mut form.tag),
        1 => Some(&mut form.path),
        _ => None,
    }
}
