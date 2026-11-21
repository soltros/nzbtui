use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/soltros/nzbtui/releases/latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Nix,
    Deb,
    Rpm,
    ArchPacman,
    AlpineApk,
    Standalone,
}

pub fn alpine_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "x86",
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => other,
    }
}

pub fn standalone_asset_name() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "nzbtui-linux-x86-musl",
        "aarch64" => "nzbtui-linux-aarch64",
        _ => "nzbtui-linux-x86_64",
    }
}

#[derive(Debug, Clone)]
pub struct UpdateOutcome {
    #[allow(dead_code)]
    pub latest_version: String,
    #[allow(dead_code)]
    pub updated: bool,
    pub message: String,
}

pub fn detect_install_method(exe_path: &Path) -> InstallMethod {
    let exe_str = exe_path.to_string_lossy();
    if exe_str.starts_with("/nix/store/") {
        return InstallMethod::Nix;
    }
    if command_succeeds("dpkg", &["-S", &exe_str]) {
        return InstallMethod::Deb;
    }
    if command_succeeds("rpm", &["-qf", &exe_str]) {
        return InstallMethod::Rpm;
    }
    if command_succeeds("pacman", &["-Qo", &exe_str]) {
        return InstallMethod::ArchPacman;
    }
    if command_succeeds("apk", &["info", "--who-owns", &exe_str]) {
        return InstallMethod::AlpineApk;
    }
    InstallMethod::Standalone
}

fn command_succeeds(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn parse_semver_tuple(v: &str) -> Option<(u64, u64, u64)> {
    let clean = v.trim().trim_start_matches(['v', 'V']);
    let core = clean.split('-').next()?;
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver_tuple(current), parse_semver_tuple(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

pub async fn check_and_self_update(apply: bool, is_cli: bool) -> Result<UpdateOutcome> {
    let current = env!("CARGO_PKG_VERSION");
    let client = Client::builder()
        .user_agent(concat!("nzbtui/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    let resp = client
        .get(GITHUB_LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("Failed to query GitHub releases API")?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "GitHub API returned HTTP {} while checking for updates",
            resp.status()
        ));
    }

    let body = resp
        .bytes()
        .await
        .context("Failed reading GitHub release response")?;
    let release: Value =
        serde_json::from_slice(&body).context("Failed to parse GitHub release JSON")?;
    let tag_name = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing tag_name in GitHub release"))?
        .to_string();

    if !is_newer_version(current, &tag_name) {
        return Ok(UpdateOutcome {
            latest_version: tag_name,
            updated: false,
            message: format!("Already on latest version (v{})", current),
        });
    }

    let exe_path = std::env::current_exe().context("Cannot determine current executable path")?;
    let method = detect_install_method(&exe_path);

    if !apply {
        return Ok(UpdateOutcome {
            latest_version: tag_name.clone(),
            updated: false,
            message: format!("Update available: {} (current v{})", tag_name, current),
        });
    }

    let assets = release
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("No release assets found for {}", tag_name))?;

    match method {
        InstallMethod::Nix => Ok(UpdateOutcome {
            latest_version: tag_name.clone(),
            updated: false,
            message: format!(
                "Update {} available (Nix install) — run: nix profile upgrade nzbtui",
                tag_name
            ),
        }),
        InstallMethod::ArchPacman => Ok(UpdateOutcome {
            latest_version: tag_name.clone(),
            updated: false,
            message: format!(
                "Update {} available (AUR/pacman install) — run: yay -Syu nzbtui",
                tag_name
            ),
        }),
        InstallMethod::Deb => {
            if !is_cli {
                return Ok(UpdateOutcome {
                    latest_version: tag_name.clone(),
                    updated: false,
                    message: format!(
                        "Update {} available (.deb install) — exit and run: sudo nzbtui --update",
                        tag_name
                    ),
                });
            }
            let deb_url = assets
                .iter()
                .find(|a| {
                    a.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.ends_with("_amd64.deb"))
                        .unwrap_or(false)
                })
                .and_then(|a| a.get("browser_download_url"))
                .and_then(|u| u.as_str())
                .ok_or_else(|| anyhow!("No .deb asset found in release {}", tag_name))?;

            let bytes = client
                .get(deb_url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            let tmp_deb = std::env::temp_dir().join(format!("nzbtui_{}.deb", tag_name));
            fs::write(&tmp_deb, &bytes)?;

            let status = Command::new("dpkg")
                .arg("-i")
                .arg(&tmp_deb)
                .status()
                .context("Failed to invoke dpkg")?;
            let _ = fs::remove_file(&tmp_deb);

            if status.success() {
                Ok(UpdateOutcome {
                    latest_version: tag_name.clone(),
                    updated: true,
                    message: format!("Updated .deb package to {}!", tag_name),
                })
            } else {
                Err(anyhow!(
                    "dpkg -i failed — try running: sudo nzbtui --update"
                ))
            }
        }
        InstallMethod::Rpm => {
            if !is_cli {
                return Ok(UpdateOutcome {
                    latest_version: tag_name.clone(),
                    updated: false,
                    message: format!(
                        "Update {} available (RPM install) — exit and run: sudo nzbtui --update",
                        tag_name
                    ),
                });
            }
            if let Some(rpm_url) = assets
                .iter()
                .find(|a| {
                    a.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.ends_with(".rpm") && !n.ends_with(".src.rpm"))
                        .unwrap_or(false)
                })
                .and_then(|a| a.get("browser_download_url"))
                .and_then(|u| u.as_str())
            {
                let bytes = client
                    .get(rpm_url)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                let tmp_rpm = std::env::temp_dir().join(format!("nzbtui_{}.rpm", tag_name));
                fs::write(&tmp_rpm, &bytes)?;
                let status = Command::new("rpm")
                    .arg("-Uvh")
                    .arg(&tmp_rpm)
                    .status()
                    .context("Failed to invoke rpm")?;
                let _ = fs::remove_file(&tmp_rpm);
                if status.success() {
                    return Ok(UpdateOutcome {
                        latest_version: tag_name.clone(),
                        updated: true,
                        message: format!("Updated RPM package to {}!", tag_name),
                    });
                }
            }
            Err(anyhow!(
                "Update {} available — build/install RPM from https://github.com/soltros/nzbtui/releases/tag/{}",
                tag_name,
                tag_name
            ))
        }
        InstallMethod::AlpineApk => {
            if !is_cli {
                return Ok(UpdateOutcome {
                    latest_version: tag_name.clone(),
                    updated: false,
                    message: format!(
                        "Update {} available (Alpine APK) — exit and run: nzbtui --update",
                        tag_name
                    ),
                });
            }
            let arch_suffix = format!("-{}.apk", alpine_arch());
            let apk_url = assets
                .iter()
                .find(|a| {
                    a.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.ends_with(&arch_suffix))
                        .unwrap_or(false)
                })
                .and_then(|a| a.get("browser_download_url"))
                .and_then(|u| u.as_str())
                .ok_or_else(|| {
                    anyhow!(
                        "No Alpine APK asset (*{}) found in release {}",
                        arch_suffix,
                        tag_name
                    )
                })?;

            let bytes = client
                .get(apk_url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            let tmp_apk = std::env::temp_dir().join(format!(
                "nzbtui_{}_{}.apk",
                tag_name,
                alpine_arch()
            ));
            fs::write(&tmp_apk, &bytes)?;

            let status = Command::new("apk")
                .args(["add", "--allow-untrusted", "--upgrade"])
                .arg(&tmp_apk)
                .status()
                .context("Failed to invoke apk")?;
            let _ = fs::remove_file(&tmp_apk);

            if status.success() {
                Ok(UpdateOutcome {
                    latest_version: tag_name.clone(),
                    updated: true,
                    message: format!("Updated Alpine APK package to {}!", tag_name),
                })
            } else {
                Err(anyhow!(
                    "apk add failed — run as root or with doas/sudo: nzbtui --update"
                ))
            }
        }
        InstallMethod::Standalone => {
            let want_asset = standalone_asset_name();
            let bin_url = assets
                .iter()
                .find(|a| {
                    a.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n == want_asset)
                        .unwrap_or(false)
                })
                .and_then(|a| a.get("browser_download_url"))
                .and_then(|u| u.as_str())
                .ok_or_else(|| {
                    anyhow!(
                        "Release {} does not contain {} asset",
                        tag_name,
                        want_asset
                    )
                })?;

            let bytes = client
                .get(bin_url)
                .send()
                .await
                .context("Failed to download update binary")?
                .error_for_status()
                .context("Failed to download update binary")?
                .bytes()
                .await
                .context("Failed reading update binary")?;

            if bytes.len() < 4 || &bytes[..4] != b"\x7fELF" {
                return Err(anyhow!("Downloaded file is not a valid Linux ELF binary"));
            }

            let tmp_path = exe_path.with_extension("update.tmp");
            match fs::write(&tmp_path, &bytes) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755));
                    }
                    match fs::rename(&tmp_path, &exe_path) {
                        Ok(()) => Ok(UpdateOutcome {
                            latest_version: tag_name.clone(),
                            updated: true,
                            message: format!(
                                "Updated to {}! Restart nzbtui to run the new version.",
                                tag_name
                            ),
                        }),
                        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                            let _ = fs::remove_file(&tmp_path);
                            Err(anyhow!(
                                "Permission denied replacing {}. Run: sudo nzbtui --update",
                                exe_path.display()
                            ))
                        }
                        Err(e) => {
                            let _ = fs::remove_file(&tmp_path);
                            Err(anyhow!("Failed replacing {}: {}", exe_path.display(), e))
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(anyhow!(
                    "Permission denied writing to {}. Run: sudo nzbtui --update",
                    exe_path.display()
                )),
                Err(e) => Err(anyhow!("Failed writing update temp file: {}", e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semver_comparison() {
        assert_eq!(parse_semver_tuple("v0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_semver_tuple("1.2.3-beta"), Some((1, 2, 3)));
        assert!(is_newer_version("0.3.1", "v0.3.2"));
        assert!(is_newer_version("0.3.1", "v0.4.0"));
        assert!(!is_newer_version("0.3.1", "v0.3.1"));
        assert!(!is_newer_version("0.4.0", "v0.3.9"));
    }

    #[test]
    fn test_detect_install_method_nix_and_standalone() {
        assert_eq!(
            detect_install_method(Path::new("/nix/store/abc123-nzbtui-0.4.0/bin/nzbtui")),
            InstallMethod::Nix
        );
        assert_eq!(
            detect_install_method(Path::new("/home/user/.local/bin/nzbtui")),
            InstallMethod::Standalone
        );
    }
}
