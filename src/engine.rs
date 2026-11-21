use crate::config::AppConfig;
use crate::nntp::{NntpError, ProviderPool};
use crate::nzb::{FileRole, NzbDocument, NzbFile};
use crate::postproc::{self, Par2VerifyOutcome};
use crate::yenc;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, RwLock};
use uuid::Uuid;

const MAX_LOG_ENTRIES: usize = 500;
const SPEED_HISTORY_LEN: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    Downloading,
    Paused,
    Verifying,
    DownloadingRepair,
    Repairing,
    Extracting,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn label(&self) -> &'static str {
        match self {
            JobStatus::Queued => "QUEUED",
            JobStatus::Downloading => "DL",
            JobStatus::Paused => "PAUSED",
            JobStatus::Verifying => "PAR2",
            JobStatus::DownloadingRepair => "PAR2-DL",
            JobStatus::Repairing => "REPAIR",
            JobStatus::Extracting => "EXTRACT",
            JobStatus::Finalizing => "MOVE",
            JobStatus::Completed => "DONE",
            JobStatus::Failed => "FAILED",
            JobStatus::Cancelled => "CANCEL",
        }
    }

    #[allow(dead_code)]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            JobStatus::Downloading
                | JobStatus::Verifying
                | JobStatus::DownloadingRepair
                | JobStatus::Repairing
                | JobStatus::Extracting
                | JobStatus::Finalizing
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSummary {
    pub id: Uuid,
    pub title: String,
    pub category: Option<String>,
    pub status: JobStatus,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub total_segments: usize,
    pub completed_segments: usize,
    pub missing_segments: usize,
    pub files_total: usize,
    pub files_done: usize,
    pub speed_bps: u64,
    pub stage_detail: String,
    pub error: Option<String>,
    pub output_path: Option<PathBuf>,
    #[serde(default)]
    pub export_tag: Option<String>,
    #[serde(default)]
    pub exported_to: Option<PathBuf>,
    pub added_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl JobSummary {
    pub fn progress_ratio(&self) -> f64 {
        if self.status == JobStatus::Completed {
            return 1.0;
        }
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
    }

    pub fn eta_seconds(&self) -> Option<u64> {
        if self.status != JobStatus::Downloading || self.speed_bps == 0 {
            return None;
        }
        let rem = self.total_bytes.saturating_sub(self.downloaded_bytes);
        Some(rem / self.speed_bps)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed_history: usize,
    pub removed_temp_dirs: usize,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    pub queue: Vec<JobSummary>,
    pub history: Vec<JobSummary>,
    pub global_paused: bool,
    pub current_speed_bps: u64,
    pub speed_history: Vec<u64>,
    pub active_connections: usize,
    pub max_connections: usize,
    pub speed_limit_kbps: u64,
    pub free_disk_bytes: Option<u64>,
    pub logs: Vec<LogEntry>,
}

struct ManagedJob {
    summary: JobSummary,
    nzb: NzbDocument,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    bytes_counter: Arc<AtomicU64>,
    segments_done: Arc<AtomicUsize>,
    segments_missing: Arc<AtomicUsize>,
    files_done: Arc<AtomicUsize>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedHistory {
    history: Vec<JobSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedQueueItem {
    summary: JobSummary,
    nzb: NzbDocument,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedQueue {
    queue: Vec<PersistedQueueItem>,
}

pub struct DownloadEngine {
    config: Arc<RwLock<AppConfig>>,
    queue: Arc<RwLock<Vec<ManagedJob>>>,
    history: Arc<RwLock<Vec<JobSummary>>>,
    logs: Arc<RwLock<VecDeque<LogEntry>>>,
    global_paused: Arc<AtomicBool>,
    active_connections: Arc<AtomicUsize>,
    bytes_window: Arc<AtomicU64>,
    speed_history: Arc<RwLock<VecDeque<u64>>>,
    current_speed_bps: Arc<AtomicU64>,
    speed_limit_kbps: Arc<AtomicU64>,
    free_disk_bytes: Arc<AtomicU64>,
    wakeup: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    history_path: PathBuf,
    queue_path: PathBuf,
    nzb_archive_dir: PathBuf,
}

impl DownloadEngine {
    pub fn new(config: Arc<RwLock<AppConfig>>, state_dir: PathBuf) -> Arc<Self> {
        let history_path = state_dir.join("history.json");
        let queue_path = state_dir.join("queue.json");
        let nzb_archive_dir = state_dir.join("nzbs");
        let loaded_history = load_history(&history_path);
        let loaded_queue = load_queue(&queue_path);
        let initial_speed_limit = config
            .try_read()
            .map(|c| c.downloads.speed_limit_kbps)
            .unwrap_or(0);

        let engine = Arc::new(Self {
            config,
            queue: Arc::new(RwLock::new(loaded_queue)),
            history: Arc::new(RwLock::new(loaded_history)),
            logs: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))),
            global_paused: Arc::new(AtomicBool::new(false)),
            active_connections: Arc::new(AtomicUsize::new(0)),
            bytes_window: Arc::new(AtomicU64::new(0)),
            speed_history: Arc::new(RwLock::new(VecDeque::from(vec![0u64; SPEED_HISTORY_LEN]))),
            current_speed_bps: Arc::new(AtomicU64::new(0)),
            speed_limit_kbps: Arc::new(AtomicU64::new(initial_speed_limit)),
            free_disk_bytes: Arc::new(AtomicU64::new(u64::MAX)),
            wakeup: Arc::new(Notify::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            history_path,
            queue_path,
            nzb_archive_dir,
        });

        // Spawn speed sampler + watch folder + disk space monitor loop (1 Hz)
        {
            let eng = Arc::clone(&engine);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                let mut secs_elapsed: u64 = 0;
                loop {
                    tick.tick().await;
                    if eng.shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let bytes = eng.bytes_window.swap(0, Ordering::Relaxed);
                    eng.current_speed_bps.store(bytes, Ordering::Relaxed);
                    {
                        let mut hist = eng.speed_history.write().await;
                        if hist.len() >= SPEED_HISTORY_LEN {
                            hist.pop_front();
                        }
                        hist.push_back(bytes);
                    }
                    // Also sync live counters into queue summaries
                    {
                        let mut q = eng.queue.write().await;
                        for job in q.iter_mut() {
                            if job.summary.status == JobStatus::Downloading
                                || job.summary.status == JobStatus::DownloadingRepair
                            {
                                job.summary.downloaded_bytes =
                                    job.bytes_counter.load(Ordering::Relaxed);
                                job.summary.completed_segments =
                                    job.segments_done.load(Ordering::Relaxed);
                                job.summary.missing_segments =
                                    job.segments_missing.load(Ordering::Relaxed);
                                job.summary.files_done = job.files_done.load(Ordering::Relaxed);
                                job.summary.speed_bps = bytes;
                            } else {
                                job.summary.speed_bps = 0;
                            }
                        }
                    }

                    // Every 5 seconds: check disk space & poll watch folder
                    if secs_elapsed % 5 == 0 {
                        eng.check_disk_and_watch_dir().await;
                    }
                    secs_elapsed = secs_elapsed.wrapping_add(1);
                }
            });
        }

        // Spawn queue scheduler loop
        {
            let eng = Arc::clone(&engine);
            tokio::spawn(async move {
                eng.scheduler_loop().await;
            });
        }

        engine
    }

    pub async fn log(&self, level: LogLevel, message: impl Into<String>) {
        let mut logs = self.logs.write().await;
        if logs.len() >= MAX_LOG_ENTRIES {
            logs.pop_front();
        }
        logs.push_back(LogEntry {
            timestamp: Utc::now(),
            level,
            message: message.into(),
        });
    }

    pub async fn enqueue_nzb(&self, nzb: NzbDocument) -> Uuid {
        let id = Uuid::new_v4();
        let initial_files: Vec<&NzbFile> = nzb
            .files
            .iter()
            .filter(|f| f.role != FileRole::Par2Recovery)
            .collect();
        let total_bytes: u64 = initial_files.iter().map(|f| f.total_bytes()).sum();
        let total_segments: usize = initial_files.iter().map(|f| f.segments.len()).sum();

        let summary = JobSummary {
            id,
            title: nzb.title.clone(),
            category: nzb.category.clone(),
            status: JobStatus::Queued,
            total_bytes,
            downloaded_bytes: 0,
            total_segments,
            completed_segments: 0,
            missing_segments: 0,
            files_total: initial_files.len(),
            files_done: 0,
            speed_bps: 0,
            stage_detail: "Waiting in queue".to_string(),
            error: None,
            output_path: None,
            export_tag: None,
            exported_to: None,
            added_at: Utc::now(),
            finished_at: None,
        };

        let title = summary.title.clone();
        self.archive_nzb(id, &nzb);
        self.queue.write().await.push(ManagedJob {
            summary,
            nzb,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            bytes_counter: Arc::new(AtomicU64::new(0)),
            segments_done: Arc::new(AtomicUsize::new(0)),
            segments_missing: Arc::new(AtomicUsize::new(0)),
            files_done: Arc::new(AtomicUsize::new(0)),
        });

        self.persist_queue().await;
        self.log(LogLevel::Info, format!("Queued NZB: {}", title))
            .await;
        self.wakeup.notify_one();
        id
    }

    pub async fn enqueue_local_file(&self, path: &Path) -> Result<(Uuid, String)> {
        let bytes = fs::read(path).map_err(|e| anyhow!("Cannot read {}: {}", path.display(), e))?;
        let fallback = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local_import");
        let nzb = NzbDocument::parse(&bytes, fallback)?;
        let title = nzb.title.clone();
        let id = self.enqueue_nzb(nzb).await;
        Ok((id, title))
    }

    pub async fn toggle_global_pause(&self) -> bool {
        let prev = self.global_paused.fetch_xor(true, Ordering::Relaxed);
        let now_paused = !prev;
        self.log(
            LogLevel::Info,
            if now_paused {
                "Global downloads paused"
            } else {
                "Global downloads resumed"
            },
        )
        .await;
        if !now_paused {
            self.wakeup.notify_one();
        }
        now_paused
    }

    pub async fn toggle_job_pause(&self, id: Uuid) {
        {
            let mut q = self.queue.write().await;
            if let Some(job) = q.iter_mut().find(|j| j.summary.id == id) {
                let prev = job.pause_flag.fetch_xor(true, Ordering::Relaxed);
                let now_paused = !prev;
                if now_paused
                    && matches!(job.summary.status, JobStatus::Queued | JobStatus::Downloading)
                {
                    job.summary.status = JobStatus::Paused;
                    job.summary.stage_detail = "Paused by user".into();
                } else if !now_paused && job.summary.status == JobStatus::Paused {
                    job.summary.status = JobStatus::Queued;
                    job.summary.stage_detail = "Resuming…".into();
                    self.wakeup.notify_one();
                }
            }
        }
        self.persist_queue().await;
    }

    pub async fn cancel_job(&self, id: Uuid) {
        let mut removed_summary = None;
        {
            let mut q = self.queue.write().await;
            if let Some(pos) = q.iter().position(|j| j.summary.id == id) {
                q[pos].cancel_flag.store(true, Ordering::Relaxed);
                if !q[pos].summary.status.is_active() {
                    let mut job = q.remove(pos);
                    job.summary.status = JobStatus::Cancelled;
                    job.summary.finished_at = Some(Utc::now());
                    job.summary.stage_detail = "Cancelled".into();
                    removed_summary = Some(job.summary);
                } else {
                    q[pos].summary.stage_detail = "Cancelling…".into();
                }
            }
        }
        if let Some(s) = removed_summary {
            self.push_history(s).await;
            self.persist_queue().await;
        }
    }

    pub async fn move_job(&self, id: Uuid, up: bool) {
        {
            let mut q = self.queue.write().await;
            if let Some(pos) = q.iter().position(|j| j.summary.id == id) {
                if up && pos > 0 {
                    q.swap(pos, pos - 1);
                } else if !up && pos + 1 < q.len() {
                    q.swap(pos, pos + 1);
                }
            }
        }
        self.persist_queue().await;
    }

    pub async fn clear_history(&self) {
        let ids: Vec<Uuid> = self.history.read().await.iter().map(|h| h.id).collect();
        self.history.write().await.clear();
        self.persist_history().await;
        for id in ids {
            let _ = fs::remove_file(self.nzb_archive_dir.join(format!("{}.json", id)));
        }
    }

    pub async fn remove_history_item(&self, id: Uuid) {
        self.history.write().await.retain(|h| h.id != id);
        self.persist_history().await;
        let _ = fs::remove_file(self.nzb_archive_dir.join(format!("{}.json", id)));
    }

    pub async fn set_history_export_tag(&self, id: Uuid, tag: Option<String>) {
        {
            let mut hist = self.history.write().await;
            if let Some(item) = hist.iter_mut().find(|h| h.id == id) {
                item.export_tag = tag;
            }
        }
        self.persist_history().await;
    }

    /// Exports a completed history item (file or directory) into `dest_dir`.
    /// When `move_files` is true, moves the item (with cross-filesystem copy+remove fallback)
    /// and updates `output_path` to the new location.
    pub async fn export_history_item(
        &self,
        id: Uuid,
        dest_dir: &Path,
        move_files: bool,
    ) -> Result<PathBuf> {
        let (title, src) = {
            let hist = self.history.read().await;
            let item = hist
                .iter()
                .find(|h| h.id == id)
                .ok_or_else(|| anyhow!("Download item not found in history"))?;
            if item.status != JobStatus::Completed {
                return Err(anyhow!("Only completed downloads can be exported"));
            }
            let src = item
                .output_path
                .clone()
                .ok_or_else(|| anyhow!("No output path recorded for '{}'", item.title))?;
            (item.title.clone(), src)
        };

        if !src.exists() {
            return Err(anyhow!("Source path does not exist: {}", src.display()));
        }

        let file_name = src
            .file_name()
            .ok_or_else(|| anyhow!("Invalid source path: {}", src.display()))?
            .to_os_string();
        let dest_path = dest_dir.join(&file_name);

        if dest_path.exists() {
            return Err(anyhow!(
                "Destination already exists: {}",
                dest_path.display()
            ));
        }

        let src_clone = src.clone();
        let dest_dir_clone = dest_dir.to_path_buf();
        let dest_path_clone = dest_path.clone();

        tokio::task::spawn_blocking(move || {
            transfer_path(&src_clone, &dest_dir_clone, &dest_path_clone, move_files)
        })
        .await
        .map_err(|e| anyhow!("Export task failed: {}", e))??;

        {
            let mut hist = self.history.write().await;
            if let Some(item) = hist.iter_mut().find(|h| h.id == id) {
                item.exported_to = Some(dest_path.clone());
                if move_files {
                    item.output_path = Some(dest_path.clone());
                }
            }
        }
        self.persist_history().await;

        let verb = if move_files { "Moved" } else { "Copied" };
        self.log(
            LogLevel::Success,
            format!("{} '{}' -> {}", verb, title, dest_path.display()),
        )
        .await;

        Ok(dest_path)
    }

    /// Removes Failed/Cancelled history entries and deletes orphaned job temp directories
    /// in `temp_dir` that do not belong to any currently queued or active job.
    pub async fn cleanup_downloads(&self) -> Result<CleanupReport> {
        let active_ids: std::collections::HashSet<String> = {
            let q = self.queue.read().await;
            q.iter().map(|j| j.summary.id.to_string()).collect()
        };

        let removed_history = {
            let mut hist = self.history.write().await;
            let before = hist.len();
            hist.retain(|h| h.status == JobStatus::Completed);
            before.saturating_sub(hist.len())
        };
        if removed_history > 0 {
            self.persist_history().await;
        }

        let temp_dir = { self.config.read().await.downloads.temp_dir.clone() };
        let (removed_temp_dirs, freed_bytes) = tokio::task::spawn_blocking(move || {
            cleanup_orphaned_temp_dirs(&temp_dir, &active_ids)
        })
        .await
        .map_err(|e| anyhow!("Cleanup task failed: {}", e))??;

        let report = CleanupReport {
            removed_history,
            removed_temp_dirs,
            freed_bytes,
        };

        self.log(
            LogLevel::Info,
            format!(
                "Download cleanup: removed {} failed/cancelled history item(s), {} orphaned temp dir(s) ({} freed)",
                report.removed_history,
                report.removed_temp_dirs,
                format_bytes(report.freed_bytes)
            ),
        )
        .await;

        Ok(report)
    }

    pub fn set_speed_limit_kbps(&self, kbps: u64) {
        self.speed_limit_kbps.store(kbps, Ordering::Relaxed);
    }

    pub async fn retry_history_item(&self, id: Uuid) -> Result<String> {
        let archive_path = self.nzb_archive_dir.join(format!("{}.json", id));
        let raw = fs::read_to_string(&archive_path).map_err(|_| {
            anyhow!("Saved NZB payload is no longer available for this history item")
        })?;
        let nzb: NzbDocument = serde_json::from_str(&raw)
            .map_err(|e| anyhow!("Failed to parse saved NZB payload: {}", e))?;
        let title = nzb.title.clone();
        self.remove_history_item(id).await;
        self.enqueue_nzb(nzb).await;
        Ok(title)
    }

    async fn check_disk_and_watch_dir(&self) {
        let cfg = self.config.read().await.clone();

        // 1. Check free disk space
        let probe_path = if cfg.downloads.download_dir.exists() {
            cfg.downloads.download_dir.clone()
        } else {
            cfg.downloads.temp_dir.clone()
        };
        if let Ok(Some(free)) =
            tokio::task::spawn_blocking(move || available_disk_bytes(&probe_path)).await
        {
            self.free_disk_bytes.store(free, Ordering::Relaxed);
            if cfg.downloads.min_free_disk_mb > 0 {
                let threshold = cfg.downloads.min_free_disk_mb.saturating_mul(1024 * 1024);
                if free < threshold && !self.global_paused.load(Ordering::Relaxed) {
                    self.global_paused.store(true, Ordering::Relaxed);
                    self.log(
                        LogLevel::Warn,
                        format!(
                            "Low disk space ({} free < {} MiB limit) — pausing all downloads",
                            format_bytes(free),
                            cfg.downloads.min_free_disk_mb
                        ),
                    )
                    .await;
                }
            }
        }

        // 2. Poll watch_dir for *.nzb files
        if let Some(ref watch_dir) = cfg.downloads.watch_dir {
            if watch_dir.is_dir() {
                let wd = watch_dir.clone();
                let nzb_paths = tokio::task::spawn_blocking(move || {
                    let mut out = Vec::new();
                    if let Ok(entries) = fs::read_dir(&wd) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.is_file()
                                && p.extension()
                                    .and_then(|e| e.to_str())
                                    .map(|e| e.eq_ignore_ascii_case("nzb"))
                                    .unwrap_or(false)
                            {
                                out.push(p);
                            }
                        }
                    }
                    out
                })
                .await
                .unwrap_or_default();

                for path in nzb_paths {
                    match self.enqueue_local_file(&path).await {
                        Ok((_, title)) => {
                            let imported_path = path.with_extension("nzb.imported");
                            let _ = fs::rename(&path, &imported_path);
                            self.log(
                                LogLevel::Info,
                                format!("Watch folder imported: {} ({})", title, path.display()),
                            )
                            .await;
                        }
                        Err(err) => {
                            let failed_path = path.with_extension("nzb.failed");
                            let _ = fs::rename(&path, &failed_path);
                            self.log(
                                LogLevel::Error,
                                format!("Watch folder failed {}: {}", path.display(), err),
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    fn archive_nzb(&self, id: Uuid, nzb: &NzbDocument) {
        let dir = self.nzb_archive_dir.clone();
        let path = dir.join(format!("{}.json", id));
        if let Ok(json) = serde_json::to_string(nzb) {
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(path, json);
        }
    }

    async fn persist_queue(&self) {
        let items: Vec<PersistedQueueItem> = {
            let q = self.queue.read().await;
            q.iter()
                .map(|j| PersistedQueueItem {
                    summary: j.summary.clone(),
                    nzb: j.nzb.clone(),
                })
                .collect()
        };
        let path = self.queue_path.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&PersistedQueue { queue: items }) {
                let tmp = path.with_extension("json.tmp");
                if fs::write(&tmp, json).is_ok() {
                    let _ = fs::rename(&tmp, &path);
                }
            }
        })
        .await;
    }

    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let q = self.queue.read().await;
        for j in q.iter() {
            j.cancel_flag.store(true, Ordering::Relaxed);
        }
        self.wakeup.notify_waiters();
    }

    pub async fn snapshot(&self) -> EngineSnapshot {
        let queue: Vec<JobSummary> = {
            let q = self.queue.read().await;
            q.iter()
                .map(|j| {
                    let mut s = j.summary.clone();
                    if s.status == JobStatus::Downloading
                        || s.status == JobStatus::DownloadingRepair
                    {
                        s.downloaded_bytes = j.bytes_counter.load(Ordering::Relaxed);
                        s.completed_segments = j.segments_done.load(Ordering::Relaxed);
                        s.missing_segments = j.segments_missing.load(Ordering::Relaxed);
                        s.files_done = j.files_done.load(Ordering::Relaxed);
                    }
                    s
                })
                .collect()
        };
        let history = self.history.read().await.clone();
        let speed_history: Vec<u64> = self.speed_history.read().await.iter().copied().collect();
        let logs: Vec<LogEntry> = self.logs.read().await.iter().cloned().collect();
        let max_connections: usize = {
            let cfg = self.config.read().await;
            cfg.enabled_providers_sorted()
                .iter()
                .map(|p| p.connections)
                .sum()
        };

        let raw_free = self.free_disk_bytes.load(Ordering::Relaxed);
        let free_disk_bytes = if raw_free == u64::MAX {
            None
        } else {
            Some(raw_free)
        };

        EngineSnapshot {
            queue,
            history,
            global_paused: self.global_paused.load(Ordering::Relaxed),
            current_speed_bps: self.current_speed_bps.load(Ordering::Relaxed),
            speed_history,
            active_connections: self.active_connections.load(Ordering::Relaxed),
            max_connections,
            speed_limit_kbps: self.speed_limit_kbps.load(Ordering::Relaxed),
            free_disk_bytes,
            logs,
        }
    }

    async fn push_history(&self, summary: JobSummary) {
        {
            let mut hist = self.history.write().await;
            hist.insert(0, summary);
            if hist.len() > 200 {
                hist.truncate(200);
            }
        }
        self.persist_history().await;
    }

    async fn persist_history(&self) {
        let hist = self.history.read().await.clone();
        let path = self.history_path.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&PersistedHistory { history: hist }) {
                let _ = fs::write(&path, json);
            }
        })
        .await;
    }

    async fn scheduler_loop(self: Arc<Self>) {
        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                break;
            }

            if !self.global_paused.load(Ordering::Relaxed) {
                let max_jobs = self.config.read().await.downloads.max_concurrent_jobs.max(1);
                let next_job_id = {
                    let mut q = self.queue.write().await;
                    let active_count = q.iter().filter(|j| j.summary.status.is_active()).count();
                    if active_count < max_jobs {
                        q.iter_mut()
                            .find(|j| {
                                j.summary.status == JobStatus::Queued
                                    && !j.pause_flag.load(Ordering::Relaxed)
                                    && !j.cancel_flag.load(Ordering::Relaxed)
                            })
                            .map(|j| {
                                j.summary.status = JobStatus::Downloading;
                                j.summary.stage_detail = "Connecting to providers…".into();
                                j.summary.id
                            })
                    } else {
                        None
                    }
                };

                if let Some(job_id) = next_job_id {
                    let eng = Arc::clone(&self);
                    tokio::spawn(async move {
                        eng.run_job(job_id).await;
                        eng.wakeup.notify_one();
                    });
                    continue;
                }
            }

            tokio::select! {
                _ = self.wakeup.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    async fn run_job(&self, job_id: Uuid) {
        let cfg = self.config.read().await.clone();
        let providers = cfg.enabled_providers_sorted();

        if providers.is_empty() {
            self.finish_job(
                job_id,
                JobStatus::Failed,
                Some("No enabled Usenet providers configured (open Settings [3])".into()),
                None,
            )
            .await;
            return;
        }

        // Extract job details
        let (nzb, cancel_flag, pause_flag, bytes_counter, segments_done, segments_missing, files_done) = {
            let q = self.queue.read().await;
            let Some(job) = q.iter().find(|j| j.summary.id == job_id) else {
                return;
            };
            (
                job.nzb.clone(),
                Arc::clone(&job.cancel_flag),
                Arc::clone(&job.pause_flag),
                Arc::clone(&job.bytes_counter),
                Arc::clone(&job.segments_done),
                Arc::clone(&job.segments_missing),
                Arc::clone(&job.files_done),
            )
        };

        let work_dir = cfg.downloads.temp_dir.join(job_id.to_string());
        if let Err(e) = fs::create_dir_all(&work_dir) {
            self.finish_job(
                job_id,
                JobStatus::Failed,
                Some(format!("Failed creating temp dir {}: {}", work_dir.display(), e)),
                None,
            )
            .await;
            return;
        }

        // Build provider pools sorted by priority
        let pools: Vec<Arc<ProviderPool>> = providers
            .into_iter()
            .map(|p| Arc::new(ProviderPool::new(p, Arc::clone(&self.active_connections))))
            .collect();

        let total_workers: usize = pools
            .iter()
            .map(|p| p.config.connections)
            .sum::<usize>()
            .clamp(1, 64);

        self.log(
            LogLevel::Info,
            format!(
                "Starting '{}' ({} files, {} worker slots across {} provider(s))",
                nzb.title,
                nzb.files.len(),
                total_workers,
                pools.len()
            ),
        )
        .await;

        let initial_files: Vec<NzbFile> = nzb
            .files
            .iter()
            .filter(|f| f.role != FileRole::Par2Recovery)
            .cloned()
            .collect();

        let recovery_files: Vec<NzbFile> = nzb
            .files
            .iter()
            .filter(|f| f.role == FileRole::Par2Recovery)
            .cloned()
            .collect();

        // Phase 1: Download PAR2 index + regular files
        let dl_result = self
            .download_file_batch(
                job_id,
                &initial_files,
                &work_dir,
                &pools,
                total_workers,
                &cancel_flag,
                &pause_flag,
                &bytes_counter,
                &segments_done,
                &segments_missing,
                &files_done,
            )
            .await;

        if let Err(e) = dl_result {
            for p in &pools {
                p.close_all().await;
            }
            let _ = fs::remove_dir_all(&work_dir);
            let status = if cancel_flag.load(Ordering::Relaxed) {
                JobStatus::Cancelled
            } else {
                JobStatus::Failed
            };
            self.finish_job(job_id, status, Some(e.to_string()), None)
                .await;
            return;
        }

        if cancel_flag.load(Ordering::Relaxed) {
            for p in &pools {
                p.close_all().await;
            }
            let _ = fs::remove_dir_all(&work_dir);
            self.finish_job(job_id, JobStatus::Cancelled, None, None)
                .await;
            return;
        }

        let pp = cfg.downloads.post_process.clone();

        // Phase 2: PAR2 Verify + on-demand recovery volume download + Repair
        if pp.verify_par2 {
            self.update_stage(job_id, JobStatus::Verifying, "Verifying PAR2 checksums…")
                .await;
            match postproc::verify_par2(&work_dir).await {
                Ok((Par2VerifyOutcome::Clean, _)) => {
                    self.log(LogLevel::Success, format!("PAR2 verified clean: {}", nzb.title))
                        .await;
                }
                Ok((Par2VerifyOutcome::Skipped(reason), _)) => {
                    self.log(LogLevel::Info, format!("PAR2 skipped ({}): {}", nzb.title, reason))
                        .await;
                }
                Ok((Par2VerifyOutcome::RepairPossible, _)) => {
                    if pp.repair_par2 {
                        self.update_stage(job_id, JobStatus::Repairing, "Repairing with PAR2…")
                            .await;
                        if let Err(e) = postproc::repair_par2(&work_dir).await {
                            for p in &pools {
                                p.close_all().await;
                            }
                            self.finish_job(job_id, JobStatus::Failed, Some(e.to_string()), None)
                                .await;
                            return;
                        }
                        self.log(LogLevel::Success, format!("PAR2 repair succeeded: {}", nzb.title))
                            .await;
                    }
                }
                Ok((Par2VerifyOutcome::NeedMoreBlocks { blocks_needed }, _)) => {
                    if pp.repair_par2 && !recovery_files.is_empty() {
                        self.log(
                            LogLevel::Warn,
                            format!(
                                "{}: need {} PAR2 recovery blocks — downloading recovery volumes",
                                nzb.title, blocks_needed
                            ),
                        )
                        .await;
                        self.update_stage(
                            job_id,
                            JobStatus::DownloadingRepair,
                            format!("Downloading PAR2 recovery ({} blocks needed)…", blocks_needed),
                        )
                        .await;

                        // Pick enough recovery files to cover blocks_needed
                        let chosen = select_recovery_files(&recovery_files, blocks_needed);
                        {
                            let extra_bytes: u64 = chosen.iter().map(|f| f.total_bytes()).sum();
                            let extra_segs: usize = chosen.iter().map(|f| f.segments.len()).sum();
                            let mut q = self.queue.write().await;
                            if let Some(j) = q.iter_mut().find(|j| j.summary.id == job_id) {
                                j.summary.total_bytes += extra_bytes;
                                j.summary.total_segments += extra_segs;
                                j.summary.files_total += chosen.len();
                            }
                        }

                        if let Err(e) = self
                            .download_file_batch(
                                job_id,
                                &chosen,
                                &work_dir,
                                &pools,
                                total_workers,
                                &cancel_flag,
                                &pause_flag,
                                &bytes_counter,
                                &segments_done,
                                &segments_missing,
                                &files_done,
                            )
                            .await
                        {
                            for p in &pools {
                                p.close_all().await;
                            }
                            self.finish_job(job_id, JobStatus::Failed, Some(e.to_string()), None)
                                .await;
                            return;
                        }

                        self.update_stage(job_id, JobStatus::Repairing, "Repairing with PAR2…")
                            .await;
                        if let Err(e) = postproc::repair_par2(&work_dir).await {
                            for p in &pools {
                                p.close_all().await;
                            }
                            self.finish_job(job_id, JobStatus::Failed, Some(e.to_string()), None)
                                .await;
                            return;
                        }
                        self.log(LogLevel::Success, format!("PAR2 repair succeeded: {}", nzb.title))
                            .await;
                    } else {
                        self.log(
                            LogLevel::Warn,
                            format!(
                                "{}: PAR2 reports {} damaged blocks and no repair volumes available",
                                nzb.title, blocks_needed
                            ),
                        )
                        .await;
                    }
                }
                Err(e) => {
                    self.log(LogLevel::Warn, format!("PAR2 check warning: {}", e))
                        .await;
                }
            }
        }

        // Done with NNTP connections for this job
        for p in &pools {
            p.close_all().await;
        }

        // Phase 3: Extract archives
        let extract_dir = work_dir.join("_extracted");
        let mut extracted_count = 0usize;
        if pp.extract_archives {
            self.update_stage(job_id, JobStatus::Extracting, "Extracting archives…")
                .await;
            match postproc::extract_archives(&work_dir, &extract_dir, nzb.password.as_deref()).await
            {
                Ok((count, logs)) => {
                    extracted_count = count;
                    for msg in logs {
                        self.log(LogLevel::Info, msg).await;
                    }
                }
                Err(e) => {
                    self.finish_job(job_id, JobStatus::Failed, Some(e.to_string()), None)
                        .await;
                    return;
                }
            }
        }

        // Phase 4: Finalize & cleanup
        self.update_stage(job_id, JobStatus::Finalizing, "Moving files to download directory…")
            .await;
        match postproc::finalize_output(
            &work_dir,
            &extract_dir,
            extracted_count,
            &cfg.downloads.download_dir,
            &nzb.title,
            &pp,
        ) {
            Ok(final_path) => {
                self.log(
                    LogLevel::Success,
                    format!("Completed '{}' -> {}", nzb.title, final_path.display()),
                )
                .await;
                self.finish_job(job_id, JobStatus::Completed, None, Some(final_path))
                    .await;
            }
            Err(e) => {
                self.finish_job(
                    job_id,
                    JobStatus::Failed,
                    Some(format!("Finalize failed: {}", e)),
                    None,
                )
                .await;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_file_batch(
        &self,
        job_id: Uuid,
        files: &[NzbFile],
        work_dir: &Path,
        pools: &[Arc<ProviderPool>],
        total_workers: usize,
        cancel_flag: &Arc<AtomicBool>,
        pause_flag: &Arc<AtomicBool>,
        bytes_counter: &Arc<AtomicU64>,
        segments_done: &Arc<AtomicUsize>,
        segments_missing: &Arc<AtomicUsize>,
        files_done: &Arc<AtomicUsize>,
    ) -> Result<()> {
        let total_files = files.len();
        for (idx, file) in files.iter().enumerate() {
            if cancel_flag.load(Ordering::Relaxed) || self.shutdown.load(Ordering::Relaxed) {
                return Err(anyhow!("Cancelled"));
            }

            self.update_stage(
                job_id,
                JobStatus::Downloading,
                format!("File {}/{}: {}", idx + 1, total_files, file.filename),
            )
            .await;

            // Prepare segment queue for this file
            let seg_queue = Arc::new(tokio::sync::Mutex::new(VecDeque::from(
                file.segments.clone(),
            )));
            let target_path = Arc::new(work_dir.join(&file.filename));
            let resolved_name = Arc::new(tokio::sync::Mutex::new(None::<String>));
            let file_lock = Arc::new(std::sync::Mutex::new(()));
            let fatal_error: Arc<tokio::sync::Mutex<Option<String>>> =
                Arc::new(tokio::sync::Mutex::new(None));

            let worker_count = total_workers.min(file.segments.len().max(1));
            let mut handles = Vec::with_capacity(worker_count);

            for _ in 0..worker_count {
                let seg_queue = Arc::clone(&seg_queue);
                let pools = pools.to_vec();
                let cancel_flag = Arc::clone(cancel_flag);
                let pause_flag = Arc::clone(pause_flag);
                let global_paused = Arc::clone(&self.global_paused);
                let shutdown = Arc::clone(&self.shutdown);
                let bytes_counter = Arc::clone(bytes_counter);
                let bytes_window = Arc::clone(&self.bytes_window);
                let speed_limit_kbps = Arc::clone(&self.speed_limit_kbps);
                let segments_done = Arc::clone(segments_done);
                let segments_missing = Arc::clone(segments_missing);
                let target_path = Arc::clone(&target_path);
                let resolved_name = Arc::clone(&resolved_name);
                let file_lock = Arc::clone(&file_lock);
                let fatal_error = Arc::clone(&fatal_error);

                handles.push(tokio::spawn(async move {
                    loop {
                        if cancel_flag.load(Ordering::Relaxed)
                            || shutdown.load(Ordering::Relaxed)
                            || fatal_error.lock().await.is_some()
                        {
                            break;
                        }

                        while pause_flag.load(Ordering::Relaxed)
                            || global_paused.load(Ordering::Relaxed)
                        {
                            if cancel_flag.load(Ordering::Relaxed)
                                || shutdown.load(Ordering::Relaxed)
                            {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }

                        loop {
                            let limit_kbps = speed_limit_kbps.load(Ordering::Relaxed);
                            if limit_kbps == 0 {
                                break;
                            }
                            let limit_bps = limit_kbps.saturating_mul(1024);
                            if bytes_window.load(Ordering::Relaxed) < limit_bps {
                                break;
                            }
                            if cancel_flag.load(Ordering::Relaxed)
                                || shutdown.load(Ordering::Relaxed)
                            {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }

                        let seg = {
                            let mut guard = seg_queue.lock().await;
                            guard.pop_front()
                        };
                        let Some(seg) = seg else {
                            break;
                        };

                        match fetch_segment_with_fallback(&pools, &seg.message_id).await {
                            Ok(raw_body) => {
                                let raw_len = raw_body.len() as u64;
                                bytes_counter.fetch_add(seg.bytes.max(raw_len), Ordering::Relaxed);
                                bytes_window.fetch_add(raw_len, Ordering::Relaxed);

                                match yenc::decode(&raw_body) {
                                    Ok(decoded) => {
                                        if !decoded.crc_valid {
                                            segments_missing.fetch_add(1, Ordering::Relaxed);
                                            segments_done.fetch_add(1, Ordering::Relaxed);
                                            continue;
                                        }
                                        {
                                            let mut rn = resolved_name.lock().await;
                                            if rn.is_none() && !decoded.filename.is_empty() {
                                                *rn = Some(crate::nzb::sanitize_filename(
                                                    &decoded.filename,
                                                ));
                                            }
                                        }
                                        let path = (*target_path).clone();
                                        let lock = Arc::clone(&file_lock);
                                        let offset = decoded.offset;
                                        let data = decoded.data;
                                        let write_res = tokio::task::spawn_blocking(move || {
                                            let _g = lock.lock().unwrap();
                                            let mut f = OpenOptions::new()
                                                .create(true)
                                                .write(true)
                                                .open(&path)?;
                                            f.seek(SeekFrom::Start(offset))?;
                                            f.write_all(&data)?;
                                            Ok::<(), std::io::Error>(())
                                        })
                                        .await;

                                        if let Ok(Err(e)) = write_res {
                                            let mut fe = fatal_error.lock().await;
                                            if fe.is_none() {
                                                *fe = Some(format!("Disk write error: {}", e));
                                            }
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        segments_missing.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                segments_done.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(SegmentFetchError::Missing) => {
                                segments_missing.fetch_add(1, Ordering::Relaxed);
                                segments_done.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(SegmentFetchError::Fatal(msg)) => {
                                let mut fe = fatal_error.lock().await;
                                if fe.is_none() {
                                    *fe = Some(msg);
                                }
                                break;
                            }
                        }
                    }
                }));
            }

            for h in handles {
                let _ = h.await;
            }

            if let Some(err) = fatal_error.lock().await.take() {
                return Err(anyhow!(err));
            }

            // Rename file if yEnc header gave a cleaner/true filename (e.g. obfuscated NZB subjects!)
            if let Some(true_name) = resolved_name.lock().await.take() {
                if !true_name.is_empty() && true_name != file.filename && target_path.exists() {
                    let dest = work_dir.join(&true_name);
                    if !dest.exists() {
                        let _ = fs::rename(&*target_path, &dest);
                    }
                }
            }

            files_done.fetch_add(1, Ordering::Relaxed);
        }

        Ok(())
    }

    async fn update_stage(&self, job_id: Uuid, status: JobStatus, detail: impl Into<String>) {
        let mut q = self.queue.write().await;
        if let Some(j) = q.iter_mut().find(|j| j.summary.id == job_id) {
            if !j.pause_flag.load(Ordering::Relaxed) {
                j.summary.status = status;
            }
            j.summary.stage_detail = detail.into();
        }
    }

    async fn finish_job(
        &self,
        job_id: Uuid,
        status: JobStatus,
        error: Option<String>,
        output_path: Option<PathBuf>,
    ) {
        let finished = {
            let mut q = self.queue.write().await;
            if let Some(pos) = q.iter().position(|j| j.summary.id == job_id) {
                let mut job = q.remove(pos);
                job.summary.status = status;
                job.summary.downloaded_bytes = job.bytes_counter.load(Ordering::Relaxed);
                job.summary.completed_segments = job.segments_done.load(Ordering::Relaxed);
                job.summary.missing_segments = job.segments_missing.load(Ordering::Relaxed);
                job.summary.files_done = job.files_done.load(Ordering::Relaxed);
                job.summary.speed_bps = 0;
                job.summary.error = error.clone();
                job.summary.output_path = output_path;
                job.summary.finished_at = Some(Utc::now());
                job.summary.stage_detail = match status {
                    JobStatus::Completed => "Completed".into(),
                    JobStatus::Cancelled => "Cancelled".into(),
                    JobStatus::Failed => error.clone().unwrap_or_else(|| "Failed".into()),
                    _ => String::new(),
                };
                Some(job.summary)
            } else {
                None
            }
        };

        if let Some(mut summary) = finished {
            self.persist_queue().await;
            if status == JobStatus::Failed {
                self.log(
                    LogLevel::Error,
                    format!(
                        "Job '{}' failed: {}",
                        summary.title,
                        summary.error.as_deref().unwrap_or("unknown")
                    ),
                )
                .await;
            }

            let mut auto_export_dest: Option<PathBuf> = None;
            if status == JobStatus::Completed {
                let cfg = self.config.read().await;
                if summary.export_tag.is_none() {
                    if let Some(target) =
                        cfg.match_export_target_for_category(summary.category.as_deref())
                    {
                        summary.export_tag = Some(target.tag.clone());
                    }
                }
                if cfg.downloads.auto_export {
                    if let Some(tag) = summary.export_tag.as_deref() {
                        auto_export_dest = cfg.export_path_for_tag(tag).cloned();
                    }
                }
            }

            self.push_history(summary).await;

            if let Some(dest_dir) = auto_export_dest {
                if let Err(err) = self.export_history_item(job_id, &dest_dir, true).await {
                    self.log(
                        LogLevel::Warn,
                        format!("Auto-export failed: {}", err),
                    )
                    .await;
                }
            }
        }
    }
}

enum SegmentFetchError {
    Missing,
    Fatal(String),
}

async fn fetch_segment_with_fallback(
    pools: &[Arc<ProviderPool>],
    message_id: &str,
) -> Result<Vec<u8>, SegmentFetchError> {
    let mut last_fatal: Option<String> = None;

    for pool in pools {
        let mut attempts = 0u8;
        loop {
            attempts += 1;
            match pool.fetch_body(message_id).await {
                Ok(data) => return Ok(data),
                Err(NntpError::ArticleNotFound) => {
                    // Try next backup provider immediately
                    break;
                }
                Err(NntpError::AuthFailed(msg)) => {
                    last_fatal = Some(format!(
                        "Auth failed on '{}': {}",
                        pool.config.name, msg
                    ));
                    break;
                }
                Err(NntpError::TooManyConnections(_)) => {
                    if attempts >= 4 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(400 * attempts as u64)).await;
                }
                Err(NntpError::Io(e)) | Err(NntpError::Protocol(e)) => {
                    if attempts >= 3 {
                        last_fatal = Some(format!("{}: {}", pool.config.name, e));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(300 * attempts as u64)).await;
                }
                Err(NntpError::Timeout) => {
                    if attempts >= 3 {
                        last_fatal = Some(format!("{}: timed out", pool.config.name));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        }
    }

    if let Some(fatal) = last_fatal {
        // Only treat auth failure as hard fatal; transient errors across all servers count as missing
        if fatal.starts_with("Auth failed") {
            return Err(SegmentFetchError::Fatal(fatal));
        }
    }
    Err(SegmentFetchError::Missing)
}

/// Select the smallest subset of `.volXX+YY.par2` recovery files providing at least `blocks_needed` blocks.
fn select_recovery_files(recovery_files: &[NzbFile], blocks_needed: usize) -> Vec<NzbFile> {
    let re = regex::Regex::new(r"(?i)\.vol\d+\+(\d+)\.par2$").unwrap();
    let mut scored: Vec<(usize, &NzbFile)> = recovery_files
        .iter()
        .map(|f| {
            let blocks = re
                .captures(&f.filename)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(1);
            (blocks, f)
        })
        .collect();
    scored.sort_by_key(|(b, _)| *b);

    let mut chosen = Vec::new();
    let mut accum = 0usize;
    for (blocks, f) in scored {
        chosen.push(f.clone());
        accum += blocks;
        if accum >= blocks_needed {
            break;
        }
    }
    chosen
}

fn load_history(path: &Path) -> Vec<JobSummary> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<PersistedHistory>(&raw)
        .map(|h| h.history)
        .unwrap_or_default()
}

fn load_queue(path: &Path) -> Vec<ManagedJob> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<PersistedQueue>(&raw) else {
        return Vec::new();
    };
    parsed
        .queue
        .into_iter()
        .map(|item| {
            let mut summary = item.summary;
            let is_paused = summary.status == JobStatus::Paused;
            if !is_paused {
                summary.status = JobStatus::Queued;
                summary.stage_detail = "Restored from previous session".to_string();
            }
            summary.speed_bps = 0;
            ManagedJob {
                summary,
                nzb: item.nzb,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                pause_flag: Arc::new(AtomicBool::new(is_paused)),
                bytes_counter: Arc::new(AtomicU64::new(0)),
                segments_done: Arc::new(AtomicUsize::new(0)),
                segments_missing: Arc::new(AtomicUsize::new(0)),
                files_done: Arc::new(AtomicUsize::new(0)),
            }
        })
        .collect()
}

fn available_disk_bytes(path: &Path) -> Option<u64> {
    let target = if path.exists() {
        path
    } else {
        path.parent()?
    };
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(target)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let data_line = stdout.lines().nth(1)?;
    let avail_kb: u64 = data_line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_kb.saturating_mul(1024))
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TiB", b / TB)
    } else if b >= GB {
        format!("{:.2} GiB", b / GB)
    } else if b >= MB {
        format!("{:.1} MiB", b / MB)
    } else if b >= KB {
        format!("{:.1} KiB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_speed(bps: u64) -> String {
    if bps == 0 {
        "0 B/s".to_string()
    } else {
        format!("{}/s", format_bytes(bps))
    }
}

pub fn format_eta(secs: Option<u64>) -> String {
    let Some(s) = secs else {
        return "--:--".to_string();
    };
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else {
        format!("{:02}:{:02}", m, sec)
    }
}

// Re-export Instant silence unused warning if needed
#[allow(dead_code)]
fn _unused(_: Instant) {}

fn transfer_path(src: &Path, dest_dir: &Path, dest_path: &Path, move_files: bool) -> Result<()> {
    fs::create_dir_all(dest_dir)
        .map_err(|e| anyhow!("Failed to create export directory {}: {}", dest_dir.display(), e))?;

    if move_files {
        if fs::rename(src, dest_path).is_ok() {
            return Ok(());
        }
        // Fallback for cross-device / cross-filesystem moves
        copy_path_recursive(src, dest_path)?;
        remove_path_recursive(src)?;
    } else {
        copy_path_recursive(src, dest_path)?;
    }
    Ok(())
}

fn copy_path_recursive(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_dst = dst.join(entry.file_name());
            copy_path_recursive(&entry.path(), &child_dst)?;
        }
    } else {
        fs::copy(src, dst).map_err(|e| {
            anyhow!(
                "Failed to copy {} to {}: {}",
                src.display(),
                dst.display(),
                e
            )
        })?;
    }
    Ok(())
}

fn remove_path_recursive(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn dir_size_recursive(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            total = total.saturating_add(dir_size_recursive(&p));
        } else if let Ok(meta) = entry.metadata() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn cleanup_orphaned_temp_dirs(
    temp_dir: &Path,
    active_ids: &std::collections::HashSet<String>,
) -> Result<(usize, u64)> {
    if !temp_dir.exists() {
        return Ok((0, 0));
    }
    let mut removed = 0usize;
    let mut freed = 0u64;
    for entry in fs::read_dir(temp_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if Uuid::parse_str(&name).is_ok() && !active_ids.contains(&name) {
            let size = dir_size_recursive(&path);
            if fs::remove_dir_all(&path).is_ok() {
                removed += 1;
                freed = freed.saturating_add(size);
            }
        }
    }
    Ok((removed, freed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_path_move_and_copy_file() {
        let tmp = std::env::temp_dir().join(format!("nzbtui_test_{}", Uuid::new_v4()));
        let src_dir = tmp.join("src");
        let dst_dir = tmp.join("dst");
        fs::create_dir_all(&src_dir).unwrap();

        let file_a = src_dir.join("movie.mkv");
        fs::write(&file_a, b"video-data").unwrap();

        // Copy mode keeps source
        let copied_dst = dst_dir.join("movie.mkv");
        transfer_path(&file_a, &dst_dir, &copied_dst, false).unwrap();
        assert!(file_a.exists());
        assert_eq!(fs::read(&copied_dst).unwrap(), b"video-data");

        // Move mode removes source
        let dst_dir2 = tmp.join("dst2");
        let moved_dst = dst_dir2.join("movie.mkv");
        transfer_path(&file_a, &dst_dir2, &moved_dst, true).unwrap();
        assert!(!file_a.exists());
        assert_eq!(fs::read(&moved_dst).unwrap(), b"video-data");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_transfer_path_directory() {
        let tmp = std::env::temp_dir().join(format!("nzbtui_test_{}", Uuid::new_v4()));
        let src_folder = tmp.join("My.Movie.2026");
        fs::create_dir_all(src_folder.join("subs")).unwrap();
        fs::write(src_folder.join("movie.mkv"), b"mkv").unwrap();
        fs::write(src_folder.join("subs").join("en.srt"), b"srt").unwrap();

        let dst_dir = tmp.join("movies");
        let dst_folder = dst_dir.join("My.Movie.2026");
        transfer_path(&src_folder, &dst_dir, &dst_folder, true).unwrap();

        assert!(!src_folder.exists());
        assert_eq!(fs::read(dst_folder.join("movie.mkv")).unwrap(), b"mkv");
        assert_eq!(
            fs::read(dst_folder.join("subs").join("en.srt")).unwrap(),
            b"srt"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_orphaned_temp_dirs() {
        let tmp = std::env::temp_dir().join(format!("nzbtui_cleanup_test_{}", Uuid::new_v4()));
        let active_id = Uuid::new_v4();
        let stale_id = Uuid::new_v4();

        let active_dir = tmp.join(active_id.to_string());
        let stale_dir = tmp.join(stale_id.to_string());
        let non_uuid_dir = tmp.join("keep_me");
        fs::create_dir_all(&active_dir).unwrap();
        fs::create_dir_all(&stale_dir).unwrap();
        fs::create_dir_all(&non_uuid_dir).unwrap();

        fs::write(active_dir.join("part1"), b"12345").unwrap();
        fs::write(stale_dir.join("part1"), b"1234567890").unwrap();

        let mut active_set = std::collections::HashSet::new();
        active_set.insert(active_id.to_string());

        let (removed, freed) = cleanup_orphaned_temp_dirs(&tmp, &active_set).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(freed, 10);
        assert!(active_dir.exists());
        assert!(!stale_dir.exists());
        assert!(non_uuid_dir.exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
