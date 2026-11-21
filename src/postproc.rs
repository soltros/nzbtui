use crate::config::PostProcessConfig;
use crate::nzb::{is_obfuscated_name, sanitize_filename};
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Par2VerifyOutcome {
    /// All files verified clean; no repair needed.
    Clean,
    /// Repair is needed and enough recovery blocks are already present locally.
    RepairPossible,
    /// Repair is needed and we need `blocks_needed` additional PAR2 recovery blocks.
    NeedMoreBlocks { blocks_needed: usize },
    /// No `.par2` index file present or `par2` tool unavailable.
    Skipped(String),
}

pub fn which_binary(candidates: &[&str]) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for &bin in candidates {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(bin);
            if full.is_file() {
                return Some(bin.to_string());
            }
        }
    }
    None
}

pub fn find_par2_index_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    static VOL_RE: OnceLock<Regex> = OnceLock::new();
    let vol_re = VOL_RE.get_or_init(|| Regex::new(r"(?i)\.vol\d+\+\d+\.par2$").unwrap());

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    let lower = name.to_lowercase();
                    if lower.ends_with(".par2") && !vol_re.is_match(&lower) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Run `par2 v` on `.par2` index files in `work_dir`.
pub async fn verify_par2(work_dir: &Path) -> Result<(Par2VerifyOutcome, String)> {
    let Some(par2_bin) = which_binary(&["par2", "par2verify"]) else {
        return Ok((
            Par2VerifyOutcome::Skipped("par2 binary not installed (install `par2`)".into()),
            String::new(),
        ));
    };

    let indices = find_par2_index_files(work_dir);
    if indices.is_empty() {
        return Ok((
            Par2VerifyOutcome::Skipped("No .par2 index files in release".into()),
            String::new(),
        ));
    }

    let mut worst_needed = 0usize;
    let mut repair_possible = false;
    let mut combined_log = String::new();

    for idx in indices {
        let output = Command::new(&par2_bin)
            .arg("v")
            .arg("-q")
            .arg(&idx)
            .current_dir(work_dir)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("Failed to invoke {} v", par2_bin))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        combined_log.push_str(&stdout);
        combined_log.push_str(&stderr);

        if output.status.success() {
            continue;
        }

        // par2cmdline exit codes:
        // 1 = Repair is possible with current recovery blocks
        // 2 = Repair is not possible (need more recovery blocks)
        let code = output.status.code().unwrap_or(2);
        if code == 1 {
            repair_possible = true;
        } else {
            let needed = parse_blocks_needed(&stdout).or_else(|| parse_blocks_needed(&stderr));
            worst_needed = worst_needed.max(needed.unwrap_or(10));
        }
    }

    if worst_needed > 0 {
        Ok((
            Par2VerifyOutcome::NeedMoreBlocks {
                blocks_needed: worst_needed,
            },
            combined_log,
        ))
    } else if repair_possible {
        Ok((Par2VerifyOutcome::RepairPossible, combined_log))
    } else {
        Ok((Par2VerifyOutcome::Clean, combined_log))
    }
}

fn parse_blocks_needed(text: &str) -> Option<usize> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"You need (\d+) more recovery blocks").unwrap());
    re.captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<usize>().ok())
}

/// Run `par2 r` on `.par2` index files in `work_dir`.
pub async fn repair_par2(work_dir: &Path) -> Result<String> {
    let par2_bin = which_binary(&["par2"])
        .ok_or_else(|| anyhow!("par2 binary not found for repair"))?;
    let indices = find_par2_index_files(work_dir);
    let mut combined = String::new();

    for idx in indices {
        let output = Command::new(&par2_bin)
            .arg("r")
            .arg("-q")
            .arg(&idx)
            .current_dir(work_dir)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("Failed to invoke {} r", par2_bin))?;

        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        if !output.status.success() {
            return Err(anyhow!(
                "PAR2 repair failed for {}: {}",
                idx.file_name().unwrap_or_default().to_string_lossy(),
                combined.lines().last().unwrap_or("unknown error")
            ));
        }
    }
    Ok(combined)
}

/// Extract archives in `work_dir` into `extract_dir`. Returns count of archives extracted.
pub async fn extract_archives(
    work_dir: &Path,
    extract_dir: &Path,
    password: Option<&str>,
) -> Result<(usize, Vec<String>)> {
    fs::create_dir_all(extract_dir)?;
    let archives = discover_primary_archives(work_dir);
    if archives.is_empty() {
        return Ok((0, Vec::new()));
    }

    let mut extracted = 0usize;
    let mut logs = Vec::new();

    for archive in archives {
        let name = archive
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_lowercase();

        let result = if name.ends_with(".rar") || is_old_rar_first(&name) {
            extract_rar(&archive, extract_dir, password).await
        } else if name.ends_with(".7z") || name.ends_with(".7z.001") {
            extract_7z(&archive, extract_dir, password).await
        } else if name.ends_with(".zip") {
            extract_zip(&archive, extract_dir, password).await
        } else if name.ends_with(".tar.gz")
            || name.ends_with(".tgz")
            || name.ends_with(".tar.xz")
            || name.ends_with(".tar.bz2")
            || name.ends_with(".tar")
        {
            extract_tar(&archive, extract_dir).await
        } else {
            continue;
        };

        match result {
            Ok(msg) => {
                extracted += 1;
                logs.push(msg);
            }
            Err(e) => {
                return Err(anyhow!(
                    "Extraction failed for {}: {}",
                    archive.file_name().unwrap_or_default().to_string_lossy(),
                    e
                ));
            }
        }
    }

    Ok((extracted, logs))
}

fn discover_primary_archives(dir: &Path) -> Vec<PathBuf> {
    static PART_RAR_RE: OnceLock<Regex> = OnceLock::new();
    let part_rar = PART_RAR_RE.get_or_init(|| Regex::new(r"(?i)\.part(\d+)\.rar$").unwrap());

    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    let mut archives = Vec::new();
    for path in entries {
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let lower = name.to_lowercase();

        if lower.ends_with(".rar") {
            if let Some(caps) = part_rar.captures(&lower) {
                let num: u32 = caps[1].parse().unwrap_or(0);
                if num == 1 {
                    archives.push(path);
                }
            } else {
                archives.push(path);
            }
        } else if lower.ends_with(".7z")
            || lower.ends_with(".7z.001")
            || lower.ends_with(".zip")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".tgz")
            || lower.ends_with(".tar.xz")
            || lower.ends_with(".tar.bz2")
            || lower.ends_with(".tar")
        {
            archives.push(path);
        }
    }
    archives
}

fn is_old_rar_first(_lower: &str) -> bool {
    false
}

async fn extract_rar(archive: &Path, dest: &Path, password: Option<&str>) -> Result<String> {
    if let Some(bin) = which_binary(&["unrar"]) {
        let mut cmd = Command::new(&bin);
        cmd.arg("x").arg("-o+").arg("-y");
        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        } else {
            cmd.arg("-p-");
        }
        cmd.arg(archive)
            .arg(format!("{}/", dest.display()))
            .stdin(Stdio::null());
        let out = cmd.output().await?;
        if out.status.success() {
            return Ok(format!("Extracted via {} ({})", bin, archive.display()));
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let std = String::from_utf8_lossy(&out.stdout);
        return Err(anyhow!("{}{}", err.trim(), std.trim()));
    }

    if let Some(bin) = which_binary(&["7z", "7zz", "7za"]) {
        return extract_7z_with_bin(&bin, archive, dest, password).await;
    }

    if let Some(bin) = which_binary(&["unrar-free"]) {
        let mut cmd = Command::new(&bin);
        cmd.arg("-x").arg("-f");
        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        }
        cmd.arg(archive).arg(dest).stdin(Stdio::null());
        let out = cmd.output().await?;
        if out.status.success() {
            return Ok(format!("Extracted via {}", bin));
        }
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    Err(anyhow!(
        "No RAR extractor found (install `unrar` or `7zip`/`p7zip-full`)"
    ))
}

async fn extract_7z(archive: &Path, dest: &Path, password: Option<&str>) -> Result<String> {
    let bin = which_binary(&["7z", "7zz", "7za"])
        .ok_or_else(|| anyhow!("No 7z binary found (install `7zip` or `p7zip-full`)"))?;
    extract_7z_with_bin(&bin, archive, dest, password).await
}

async fn extract_7z_with_bin(
    bin: &str,
    archive: &Path,
    dest: &Path,
    password: Option<&str>,
) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("x").arg("-y").arg(format!("-o{}", dest.display()));
    if let Some(pw) = password {
        cmd.arg(format!("-p{}", pw));
    } else {
        cmd.arg("-p");
    }
    cmd.arg(archive).stdin(Stdio::null());
    let out = cmd.output().await?;
    if out.status.success() {
        Ok(format!(
            "Extracted {} via {}",
            archive.file_name().unwrap_or_default().to_string_lossy(),
            bin
        ))
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        };
        Err(anyhow!("{}", msg))
    }
}

async fn extract_zip(archive: &Path, dest: &Path, password: Option<&str>) -> Result<String> {
    if let Some(bin) = which_binary(&["unzip"]) {
        let mut cmd = Command::new(&bin);
        cmd.arg("-o");
        if let Some(pw) = password {
            cmd.arg("-P").arg(pw);
        }
        cmd.arg(archive)
            .arg("-d")
            .arg(dest)
            .stdin(Stdio::null());
        let out = cmd.output().await?;
        if out.status.success() {
            return Ok(format!(
                "Extracted {} via unzip",
                archive.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    extract_7z(archive, dest, password).await
}

async fn extract_tar(archive: &Path, dest: &Path) -> Result<String> {
    let bin = which_binary(&["tar"]).ok_or_else(|| anyhow!("`tar` binary not found"))?;
    let out = Command::new(&bin)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .stdin(Stdio::null())
        .output()
        .await?;
    if out.status.success() {
        Ok(format!(
            "Extracted {} via tar",
            archive.file_name().unwrap_or_default().to_string_lossy()
        ))
    } else {
        Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Move final payload into `download_dir` (optionally in a job subfolder), cleaning up archives/par2 if configured.
pub fn finalize_output(
    work_dir: &Path,
    extract_dir: &Path,
    extracted_count: usize,
    download_dir: &Path,
    job_title: &str,
    pp: &PostProcessConfig,
) -> Result<PathBuf> {
    let folder_name = {
        let clean = sanitize_filename(job_title);
        if clean.is_empty() {
            "download".to_string()
        } else {
            clean
        }
    };

    let final_dir = if pp.create_job_subfolder {
        unique_dir(download_dir.join(&folder_name))
    } else {
        fs::create_dir_all(download_dir)?;
        download_dir.to_path_buf()
    };
    fs::create_dir_all(&final_dir)?;

    if extracted_count > 0 && extract_dir.exists() {
        move_dir_contents(extract_dir, &final_dir)?;
        // Also copy non-archive, non-par2 companion files (like .nfo/.srt) if not cleaned up or useful
        if !pp.cleanup_after_extract {
            move_dir_contents(work_dir, &final_dir)?;
        } else {
            // Keep non-archive, non-par2 files like .nfo, .mkv, .srt if present alongside archives
            for entry in fs::read_dir(work_dir)?.flatten() {
                let p = entry.path();
                if p == extract_dir {
                    continue;
                }
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if !is_archive_or_par2(name) {
                        let dst = final_dir.join(name);
                        if !dst.exists() {
                            move_entry(&p, &dst)?;
                        }
                    }
                }
            }
        }
    } else {
        // No archives were extracted; move downloaded files (stripping par2 if cleanup enabled)
        for entry in fs::read_dir(work_dir)?.flatten() {
            let p = entry.path();
            if p == extract_dir {
                continue;
            }
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if pp.cleanup_after_extract && name.to_lowercase().ends_with(".par2") {
                    continue;
                }
                let dst = final_dir.join(name);
                move_entry(&p, &dst)?;
            }
        }
    }

    rename_obfuscated_files(&final_dir, &folder_name);

    // Remove temporary working directory
    let _ = fs::remove_dir_all(work_dir);
    Ok(final_dir)
}

/// If `final_dir` contains obfuscated filenames (e.g. `350127o75I6o3L1o5w08w68K5y275182.mkv`)
/// and `clean_title` is a readable release name, rename them to `<clean_title>.<ext>`.
fn rename_obfuscated_files(dir: &Path, clean_title: &str) {
    if clean_title.is_empty() || is_obfuscated_name(clean_title) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_obfuscated_name(name) {
            continue;
        }
        let target_name = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if !ext.is_empty() => format!("{}.{}", clean_title, ext),
            _ => clean_title.to_string(),
        };
        let mut dst = dir.join(&target_name);
        if dst.exists() {
            let mut n = 2usize;
            loop {
                let candidate = match path.extension().and_then(|e| e.to_str()) {
                    Some(ext) if !ext.is_empty() => format!("{}-{}.{}", clean_title, n, ext),
                    _ => format!("{}-{}", clean_title, n),
                };
                let p = dir.join(candidate);
                if !p.exists() {
                    dst = p;
                    break;
                }
                n += 1;
            }
        }
        let _ = fs::rename(&path, &dst);
    }
}

fn is_archive_or_par2(filename: &str) -> bool {
    static OLD_RAR: OnceLock<Regex> = OnceLock::new();
    let old_rar = OLD_RAR.get_or_init(|| Regex::new(r"(?i)\.[rs]\d{2,3}$").unwrap());
    static SPLIT_7Z: OnceLock<Regex> = OnceLock::new();
    let split_7z = SPLIT_7Z.get_or_init(|| Regex::new(r"(?i)\.7z\.\d{3}$").unwrap());

    let lower = filename.to_lowercase();
    lower.ends_with(".par2")
        || lower.ends_with(".rar")
        || lower.ends_with(".7z")
        || lower.ends_with(".zip")
        || lower.ends_with(".sfv")
        || lower.ends_with(".nzb")
        || old_rar.is_match(&lower)
        || split_7z.is_match(&lower)
}

fn unique_dir(base: PathBuf) -> PathBuf {
    if !base.exists() {
        return base;
    }
    let name = base
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_string();
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    for i in 2..1000 {
        let cand = parent.join(format!("{} ({})", name, i));
        if !cand.exists() {
            return cand;
        }
    }
    base
}

fn move_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        move_entry(&from, &to)?;
    }
    Ok(())
}

fn move_entry(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Cross-filesystem fallback: copy then remove
    if from.is_dir() {
        copy_dir_recursive(from, to)?;
        fs::remove_dir_all(from)?;
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_blocks_needed() {
        let sample = "Repair is not possible.\nYou need 14 more recovery blocks to be able to repair.\n";
        assert_eq!(parse_blocks_needed(sample), Some(14));
    }

    #[test]
    fn test_is_archive_or_par2() {
        assert!(is_archive_or_par2("movie.part01.rar"));
        assert!(is_archive_or_par2("movie.r00"));
        assert!(is_archive_or_par2("movie.vol01+02.par2"));
        assert!(is_archive_or_par2("movie.7z.001"));
        assert!(!is_archive_or_par2("movie.mkv"));
        assert!(!is_archive_or_par2("movie.srt"));
    }
}
