use anyhow::{anyhow, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbDocument {
    pub title: String,
    pub password: Option<String>,
    pub category: Option<String>,
    pub meta: HashMap<String, String>,
    pub files: Vec<NzbFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileRole {
    /// Regular payload file (archives, media, nfo, etc.)
    Regular,
    /// Small `.par2` index file (not a `.volXX+YY.par2` recovery volume)
    Par2Index,
    /// PAR2 recovery volume (`.vol00+01.par2`) — downloaded on demand if repair is needed
    Par2Recovery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbFile {
    pub poster: String,
    pub date: u64,
    pub subject: String,
    pub filename: String,
    pub role: FileRole,
    pub groups: Vec<String>,
    pub segments: Vec<NzbSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbSegment {
    pub number: u32,
    pub bytes: u64,
    pub message_id: String,
}

impl NzbFile {
    pub fn total_bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.bytes).sum()
    }
}

impl NzbDocument {
    #[allow(dead_code)]
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.total_bytes()).sum()
    }

    pub fn initial_download_bytes(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| f.role != FileRole::Par2Recovery)
            .map(|f| f.total_bytes())
            .sum()
    }

    pub fn recovery_bytes(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| f.role == FileRole::Par2Recovery)
            .map(|f| f.total_bytes())
            .sum()
    }

    pub fn parse(xml_bytes: &[u8], fallback_title: &str) -> Result<Self> {
        let mut reader = Reader::from_reader(xml_bytes);
        reader.config_mut().trim_text(true);

        let mut buf = Vec::new();
        let mut meta: HashMap<String, String> = HashMap::new();
        let mut files: Vec<NzbFile> = Vec::new();

        let mut in_meta_type: Option<String> = None;
        let mut current_file: Option<NzbFile> = None;
        let mut in_group = false;
        let mut current_seg: Option<(u32, u64)> = None;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let local = e.local_name();
                    match local.as_ref() {
                        b"meta" => {
                            for attr in e.attributes().flatten() {
                                if attr.key.local_name().as_ref() == b"type" {
                                    if let Ok(val) = attr.unescape_value() {
                                        in_meta_type = Some(val.to_lowercase());
                                    }
                                }
                            }
                        }
                        b"file" => {
                            let mut poster = String::new();
                            let mut date = 0u64;
                            let mut subject = String::new();
                            for attr in e.attributes().flatten() {
                                match attr.key.local_name().as_ref() {
                                    b"poster" => {
                                        poster = attr
                                            .unescape_value()
                                            .map(|v| v.into_owned())
                                            .unwrap_or_default();
                                    }
                                    b"date" => {
                                        date = attr
                                            .unescape_value()
                                            .ok()
                                            .and_then(|v| v.parse().ok())
                                            .unwrap_or(0);
                                    }
                                    b"subject" => {
                                        subject = attr
                                            .unescape_value()
                                            .map(|v| v.into_owned())
                                            .unwrap_or_default();
                                    }
                                    _ => {}
                                }
                            }
                            let filename = extract_filename_from_subject(&subject);
                            let role = classify_file_role(&filename);
                            current_file = Some(NzbFile {
                                poster,
                                date,
                                subject,
                                filename,
                                role,
                                groups: Vec::new(),
                                segments: Vec::new(),
                            });
                        }
                        b"group" => {
                            in_group = true;
                        }
                        b"segment" => {
                            let mut number = 0u32;
                            let mut bytes = 0u64;
                            for attr in e.attributes().flatten() {
                                match attr.key.local_name().as_ref() {
                                    b"number" => {
                                        number = attr
                                            .unescape_value()
                                            .ok()
                                            .and_then(|v| v.parse().ok())
                                            .unwrap_or(0);
                                    }
                                    b"bytes" => {
                                        bytes = attr
                                            .unescape_value()
                                            .ok()
                                            .and_then(|v| v.parse().ok())
                                            .unwrap_or(0);
                                    }
                                    _ => {}
                                }
                            }
                            current_seg = Some((number, bytes));
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(ref e)) => {
                    let text = e.unescape().map(|v| v.into_owned()).unwrap_or_default();
                    if let Some(ref key) = in_meta_type {
                        if !text.is_empty() {
                            meta.insert(key.clone(), text);
                        }
                    } else if in_group {
                        if let Some(ref mut f) = current_file {
                            if !text.is_empty() {
                                f.groups.push(text);
                            }
                        }
                    } else if let Some((number, bytes)) = current_seg {
                        if let Some(ref mut f) = current_file {
                            let msg_id = text.trim().trim_matches(|c| c == '<' || c == '>');
                            if !msg_id.is_empty() {
                                f.segments.push(NzbSegment {
                                    number,
                                    bytes,
                                    message_id: msg_id.to_string(),
                                });
                            }
                        }
                    }
                }
                Ok(Event::End(ref e)) => match e.local_name().as_ref() {
                    b"meta" => in_meta_type = None,
                    b"group" => in_group = false,
                    b"segment" => current_seg = None,
                    b"file" => {
                        if let Some(mut f) = current_file.take() {
                            f.segments.sort_by_key(|s| s.number);
                            if !f.segments.is_empty() {
                                files.push(f);
                            }
                        }
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(anyhow!("Invalid NZB XML: {}", e)),
                _ => {}
            }
            buf.clear();
        }

        if files.is_empty() {
            return Err(anyhow!("NZB contains no downloadable files/segments"));
        }

        // Order files: Par2Index first (fast & tiny), then Regular, then Par2Recovery sorted by size asc
        files.sort_by_key(|f| match f.role {
            FileRole::Par2Index => (0, f.total_bytes()),
            FileRole::Regular => (1, 0),
            FileRole::Par2Recovery => (2, f.total_bytes()),
        });

        let raw_title = meta
            .get("title")
            .filter(|t| !t.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| fallback_title.to_string());

        let (clean_title, title_password) = extract_brace_password(&raw_title);
        let password = meta
            .get("password")
            .filter(|p| !p.is_empty())
            .cloned()
            .or(title_password);
        let category = meta.get("category").cloned();

        Ok(Self {
            title: if clean_title.is_empty() {
                fallback_title.to_string()
            } else {
                clean_title
            },
            password,
            category,
            meta,
            files,
        })
    }
}

/// Extract `{{password}}` convention commonly used in NZB filenames/titles.
pub fn extract_brace_password(s: &str) -> (String, Option<String>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\{\{([^}]+)\}\}").unwrap());
    if let Some(caps) = re.captures(s) {
        let pw = caps.get(1).map(|m| m.as_str().to_string());
        let cleaned = re.replace_all(s, "").trim().to_string();
        (cleaned, pw)
    } else {
        (s.trim().to_string(), None)
    }
}

/// Extract filename from standard Usenet subject headers like:
/// `[12345]-[FULL]-[#a.b.teevee]-[ Show.S01E01 ]-[01/20] - "show.s01e01.part01.rar" yEnc (1/45)`
pub fn extract_filename_from_subject(subject: &str) -> String {
    static QUOTE_RE: OnceLock<Regex> = OnceLock::new();
    let quote_re = QUOTE_RE.get_or_init(|| Regex::new(r#""([^"]+)""#).unwrap());

    if let Some(caps) = quote_re.captures(subject) {
        if let Some(m) = caps.get(1) {
            let candidate = sanitize_filename(m.as_str());
            if !candidate.is_empty() {
                return candidate;
            }
        }
    }

    // Fallback: look for token before `yEnc`
    let before_yenc = subject
        .split("yEnc")
        .next()
        .unwrap_or(subject)
        .trim();
    if let Some(token) = before_yenc.split_whitespace().last() {
        let candidate = sanitize_filename(token);
        if !candidate.is_empty() {
            return candidate;
        }
    }

    let fallback = sanitize_filename(subject);
    if fallback.is_empty() {
        "unnamed.bin".to_string()
    } else {
        fallback
    }
}

pub fn sanitize_filename(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim();
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    cleaned.trim_matches('.').trim().to_string()
}

pub fn classify_file_role(filename: &str) -> FileRole {
    let lower = filename.to_lowercase();
    if lower.ends_with(".par2") {
        static VOL_RE: OnceLock<Regex> = OnceLock::new();
        let vol_re = VOL_RE.get_or_init(|| Regex::new(r"\.vol\d+\+\d+\.par2$").unwrap());
        if vol_re.is_match(&lower) {
            FileRole::Par2Recovery
        } else {
            FileRole::Par2Index
        }
    } else {
        FileRole::Regular
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nzb_and_roles() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="title">Linux.ISO.2026{{secret123}}</meta>
    <meta type="category">PC</meta>
  </head>
  <file poster="user@example.com" date="1700000000" subject='[01/03] - "linux.part01.rar" yEnc (1/2)'>
    <groups><group>alt.binaries.misc</group></groups>
    <segments>
      <segment bytes="500000" number="2">seg2@example.com</segment>
      <segment bytes="500000" number="1">seg1@example.com</segment>
    </segments>
  </file>
  <file poster="user@example.com" date="1700000000" subject='[02/03] - "linux.par2" yEnc (1/1)'>
    <groups><group>alt.binaries.misc</group></groups>
    <segments>
      <segment bytes="12000" number="1">paridx@example.com</segment>
    </segments>
  </file>
  <file poster="user@example.com" date="1700000000" subject='[03/03] - "linux.vol00+02.par2" yEnc (1/1)'>
    <groups><group>alt.binaries.misc</group></groups>
    <segments>
      <segment bytes="250000" number="1">parvol@example.com</segment>
    </segments>
  </file>
</nzb>"#;

        let doc = NzbDocument::parse(xml, "fallback").unwrap();
        assert_eq!(doc.title, "Linux.ISO.2026");
        assert_eq!(doc.password.as_deref(), Some("secret123"));
        assert_eq!(doc.files.len(), 3);
        assert_eq!(doc.files[0].role, FileRole::Par2Index);
        assert_eq!(doc.files[1].role, FileRole::Regular);
        assert_eq!(doc.files[1].segments[0].number, 1);
        assert_eq!(doc.files[2].role, FileRole::Par2Recovery);
        assert_eq!(doc.initial_download_bytes(), 1_012_000);
        assert_eq!(doc.recovery_bytes(), 250_000);
    }
}
