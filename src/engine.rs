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

#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    pub queue: Vec<JobSummary>,
    pub history: Vec<JobSummary>,
    pub global_paused: bool,
    pub current_speed_bps: u64,
    pub speed_history: Vec<u64>,
    pub active_connections: usize,
    pub max_connections: usize,
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
    wakeup: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    history_path: PathBuf,
}

impl DownloadEngine {
    pub fn new(config: Arc<RwLock<AppConfig>>, state_dir: PathBuf) -> Arc<Self> {
        let history_path = state_dir.join("history.json");
        let loaded_history = load_history(&history_path);

        let engine = Arc::new(Self {
            config,
            queue: Arc::new(RwLock::new(Vec::new())),
            history: Arc::new(RwLock::new(loaded_history)),
            logs: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))),
            global_paused: Arc::new(AtomicBool::new(false)),
            active_connections: Arc::new(AtomicUsize::new(0)),
            bytes_window: Arc::new(AtomicU64::new(0)),
            speed_history: Arc::new(RwLock::new(VecDeque::from(vec![0u64; SPEED_HISTORY_LEN]))),
            current_speed_bps: Arc::new(AtomicU64::new(0)),
            wakeup: Arc::new(Notify::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            history_path,
        });

        // Spawn speed sampler loop (1 Hz)
        {
            let eng = Arc::clone(&engine);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
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
            added_at: Utc::now(),
            finished_at: None,
        };

        let title = summary.title.clone();
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
        let mut q = self.queue.write().await;
        if let Some(job) = q.iter_mut().find(|j| j.summary.id == id) {
            let prev = job.pause_flag.fetch_xor(true, Ordering::Relaxed);
            let now_paused = !prev;
            if now_paused && matches!(job.summary.status, JobStatus::Queued | JobStatus::Downloading) {
                job.summary.status = JobStatus::Paused;
                job.summary.stage_detail = "Paused by user".into();
            } else if !now_paused && job.summary.status == JobStatus::Paused {
                job.summary.status = JobStatus::Queued;
                job.summary.stage_detail = "Resuming…".into();
                self.wakeup.notify_one();
            }
        }
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
        }
    }

    pub async fn move_job(&self, id: Uuid, up: bool) {
        let mut q = self.queue.write().await;
        if let Some(pos) = q.iter().position(|j| j.summary.id == id) {
            if up && pos > 0 {
                q.swap(pos, pos - 1);
            } else if !up && pos + 1 < q.len() {
                q.swap(pos, pos + 1);
            }
        }
    }

    pub async fn clear_history(&self) {
        self.history.write().await.clear();
        self.persist_history().await;
    }

    pub async fn remove_history_item(&self, id: Uuid) {
        self.history.write().await.retain(|h| h.id != id);
        self.persist_history().await;
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

        EngineSnapshot {
            queue,
            history,
            global_paused: self.global_paused.load(Ordering::Relaxed),
            current_speed_bps: self.current_speed_bps.load(Ordering::Relaxed),
            speed_history,
            active_connections: self.active_connections.load(Ordering::Relaxed),
            max_connections,
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

        if let Some(summary) = finished {
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
            self.push_history(summary).await;
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
