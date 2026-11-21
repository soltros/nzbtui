use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub downloads: DownloadConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub indexers: Vec<IndexerConfig>,
    #[serde(default)]
    pub export_targets: Vec<ExportTarget>,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportTarget {
    pub tag: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadConfig {
    pub download_dir: PathBuf,
    pub temp_dir: PathBuf,
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: usize,
    #[serde(default)]
    pub post_process: PostProcessConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessConfig {
    #[serde(default = "default_true")]
    pub verify_par2: bool,
    #[serde(default = "default_true")]
    pub repair_par2: bool,
    #[serde(default = "default_true")]
    pub extract_archives: bool,
    #[serde(default = "default_true")]
    pub cleanup_after_extract: bool,
    #[serde(default = "default_true")]
    pub create_job_subfolder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub host: String,
    #[serde(default = "default_nntps_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub tls: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_connections")]
    pub connections: usize,
    /// Lower number = higher priority (0 = primary, 1+ = backup/fill)
    #[serde(default)]
    pub priority: u8,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerConfig {
    pub name: String,
    /// Base URL, e.g. https://api.nzbgeek.info or https://indexer.example/api
    pub url: String,
    pub api_key: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_tick_rate_ms")]
    pub tick_rate_ms: u64,
    #[serde(default = "default_search_limit")]
    pub search_limit: usize,
}

fn default_true() -> bool {
    true
}
fn default_max_concurrent_jobs() -> usize {
    1
}
fn default_nntps_port() -> u16 {
    563
}
fn default_connections() -> usize {
    8
}
fn default_tick_rate_ms() -> u64 {
    200
}
fn default_search_limit() -> usize {
    100
}

impl Default for PostProcessConfig {
    fn default() -> Self {
        Self {
            verify_par2: true,
            repair_par2: true,
            extract_archives: true,
            cleanup_after_extract: true,
            create_job_subfolder: true,
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        let download_dir = UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.join("nzbtui")))
            .unwrap_or_else(|| PathBuf::from("./downloads"));
        let temp_dir = ProjectDirs::from("com", "nzbtui", "nzbtui")
            .map(|p| p.data_local_dir().join("incomplete"))
            .unwrap_or_else(|| PathBuf::from("./incomplete"));
        Self {
            download_dir,
            temp_dir,
            max_concurrent_jobs: default_max_concurrent_jobs(),
            post_process: PostProcessConfig::default(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            tick_rate_ms: default_tick_rate_ms(),
            search_limit: default_search_limit(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            downloads: DownloadConfig::default(),
            providers: Vec::new(),
            indexers: Vec::new(),
            export_targets: Vec::new(),
            ui: UiConfig::default(),
        }
    }
}

impl ProviderConfig {
    pub fn new_template() -> Self {
        Self {
            name: String::from("Primary Provider"),
            host: String::new(),
            port: 563,
            tls: true,
            username: String::new(),
            password: String::new(),
            connections: 8,
            priority: 0,
            enabled: true,
        }
    }
}

impl IndexerConfig {
    pub fn new_template() -> Self {
        Self {
            name: String::from("NZBgeek"),
            url: String::from("https://api.nzbgeek.info"),
            api_key: String::new(),
            enabled: true,
        }
    }

    /// Returns normalized API endpoint URL ending with `/api` unless already present.
    pub fn api_endpoint(&self) -> String {
        let trimmed = self.url.trim().trim_end_matches('/');
        if trimmed.ends_with("/api") || trimmed.contains("/api?") {
            trimmed.to_string()
        } else {
            format!("{}/api", trimmed)
        }
    }
}

impl AppConfig {
    pub fn default_config_path() -> PathBuf {
        ProjectDirs::from("com", "nzbtui", "nzbtui")
            .map(|p| p.config_dir().join("config.json"))
            .unwrap_or_else(|| PathBuf::from("nzbtui.json"))
    }

    pub fn default_state_dir() -> PathBuf {
        ProjectDirs::from("com", "nzbtui", "nzbtui")
            .map(|p| p.data_local_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".nzbtui"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file {}", path.display()))?;
        let mut cfg: AppConfig = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse JSON config {}", path.display()))?;
        cfg.downloads.download_dir = expand_tilde(&cfg.downloads.download_dir);
        cfg.downloads.temp_dir = expand_tilde(&cfg.downloads.temp_dir);
        for target in &mut cfg.export_targets {
            target.path = expand_tilde(&target.path);
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(self).context("Failed to serialize config")?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, &content)
            .with_context(|| format!("Failed to write temp config {}", tmp_path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&tmp_path, perms).with_context(|| {
                format!("Failed to set 0600 permissions on {}", tmp_path.display())
            })?;
        }

        fs::rename(&tmp_path, path)
            .with_context(|| format!("Failed to move config into {}", path.display()))?;
        Ok(())
    }

    pub fn enabled_providers_sorted(&self) -> Vec<ProviderConfig> {
        let mut list: Vec<ProviderConfig> = self
            .providers
            .iter()
            .filter(|p| p.enabled && !p.host.trim().is_empty())
            .cloned()
            .collect();
        list.sort_by_key(|p| p.priority);
        list
    }

    pub fn enabled_indexers(&self) -> Vec<IndexerConfig> {
        self.indexers
            .iter()
            .filter(|i| i.enabled && !i.url.trim().is_empty() && !i.api_key.trim().is_empty())
            .cloned()
            .collect()
    }

    /// Returns the configured export destination path for a given tag (case-insensitive).
    pub fn export_path_for_tag(&self, tag: &str) -> Option<&PathBuf> {
        self.export_targets
            .iter()
            .find(|t| t.tag.eq_ignore_ascii_case(tag))
            .map(|t| &t.path)
    }
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(user_dirs) = UserDirs::new() {
            return user_dirs.home_dir().join(rest);
        }
    } else if s == "~" {
        if let Some(user_dirs) = UserDirs::new() {
            return user_dirs.home_dir().to_path_buf();
        }
    }
    path.to_path_buf()
}

pub fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        "(not set)".to_string()
    } else if s.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}…{}", &s[..2], &s[s.len() - 2..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indexer_endpoint_normalization() {
        let i1 = IndexerConfig {
            name: "Geek".into(),
            url: "https://api.nzbgeek.info/".into(),
            api_key: "abc".into(),
            enabled: true,
        };
        assert_eq!(i1.api_endpoint(), "https://api.nzbgeek.info/api");

        let i2 = IndexerConfig {
            name: "Slug".into(),
            url: "https://drunkenslug.com/api".into(),
            api_key: "abc".into(),
            enabled: true,
        };
        assert_eq!(i2.api_endpoint(), "https://drunkenslug.com/api");
    }

    #[test]
    fn test_config_roundtrip() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(ProviderConfig {
            name: "News".into(),
            host: "news.example.com".into(),
            port: 563,
            tls: true,
            username: "u".into(),
            password: "p".into(),
            connections: 10,
            priority: 0,
            enabled: true,
        });
        let s = serde_json::to_string_pretty(&cfg).unwrap();
        let parsed: AppConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.providers.len(), 1);
        assert_eq!(parsed.providers[0].host, "news.example.com");
    }

    #[test]
    fn test_export_targets_roundtrip_and_lookup() {
        let mut cfg = AppConfig::default();
        cfg.export_targets.push(ExportTarget {
            tag: "Movie".into(),
            path: PathBuf::from("/media/movies"),
        });
        let s = serde_json::to_string_pretty(&cfg).unwrap();
        let parsed: AppConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.export_targets.len(), 1);
        assert_eq!(
            parsed.export_path_for_tag("movie"),
            Some(&PathBuf::from("/media/movies"))
        );
        assert_eq!(parsed.export_path_for_tag("tv"), None);
    }
}
