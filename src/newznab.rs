use crate::config::IndexerConfig;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::future::join_all;
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::Client;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SearchCategory {
    pub id: &'static str,
    pub label: &'static str,
}

pub const CATEGORIES: &[SearchCategory] = &[
    SearchCategory { id: "", label: "All" },
    SearchCategory { id: "2000", label: "Movies" },
    SearchCategory { id: "5000", label: "TV" },
    SearchCategory { id: "3000", label: "Audio" },
    SearchCategory { id: "4000", label: "PC / Apps" },
    SearchCategory { id: "1000", label: "Console" },
    SearchCategory { id: "7000", label: "Books" },
    SearchCategory { id: "6000", label: "XXX" },
    SearchCategory { id: "8000", label: "Other" },
];

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub guid: String,
    pub nzb_url: String,
    pub indexer_name: String,
    pub indexer_api_key: String,
    pub category: String,
    pub size: u64,
    pub pub_date: Option<DateTime<Utc>>,
    pub grabs: u32,
    pub files: u32,
    pub password_protected: bool,
    #[allow(dead_code)]
    pub group: String,
}

impl SearchResult {
    pub fn age_display(&self) -> String {
        let Some(dt) = self.pub_date else {
            return "-".to_string();
        };
        let secs = (Utc::now() - dt).num_seconds().max(0);
        if secs < 3600 {
            format!("{}m", secs / 60)
        } else if secs < 86400 {
            format!("{}h", secs / 3600)
        } else if secs < 86400 * 365 {
            format!("{}d", secs / 86400)
        } else {
            format!("{:.1}y", secs as f64 / (86400.0 * 365.0))
        }
    }
}

pub struct NewznabClient {
    http: Client,
}

impl NewznabClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("nzbtui/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self { http })
    }

    /// Search across multiple indexers concurrently. Returns combined results + any per-indexer errors.
    pub async fn search_all(
        &self,
        indexers: &[IndexerConfig],
        query: &str,
        category_id: &str,
        season: Option<&str>,
        limit: usize,
    ) -> (Vec<SearchResult>, Vec<String>) {
        let futures = indexers
            .iter()
            .map(|idx| self.search_one(idx, query, category_id, season, limit));
        let outcomes = join_all(futures).await;

        let mut results = Vec::new();
        let mut errors = Vec::new();

        for (idx, outcome) in indexers.iter().zip(outcomes) {
            match outcome {
                Ok(mut items) => results.append(&mut items),
                Err(e) => errors.push(format!("{}: {}", idx.name, e)),
            }
        }

        // Default sort: newest first
        results.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
        (results, errors)
    }

    pub async fn search_one(
        &self,
        indexer: &IndexerConfig,
        query: &str,
        category_id: &str,
        season: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let endpoint = indexer.api_endpoint();
        let params = build_search_params(&indexer.api_key, query, category_id, season, limit);

        let resp = self
            .http
            .get(&endpoint)
            .query(&params)
            .send()
            .await
            .with_context(|| format!("Request to {} failed", endpoint))?;

        let status = resp.status();
        let body = resp.bytes().await.context("Failed reading indexer response")?;
        if !status.is_success() {
            return Err(anyhow!("HTTP {} from {}", status, indexer.name));
        }

        parse_newznab_rss(&body, &indexer.name, &indexer.api_key)
    }

    /// Download raw NZB bytes from a search result's URL.
    pub async fn fetch_nzb(&self, nzb_url: &str, api_key: &str) -> Result<Vec<u8>> {
        let mut req = self.http.get(nzb_url);
        if !api_key.is_empty() && !nzb_url.contains("apikey=") {
            req = req.query(&[("apikey", api_key)]);
        }
        let resp = req.send().await.context("Failed to download NZB")?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("Failed reading NZB body")?;
        if !status.is_success() {
            return Err(anyhow!("NZB download returned HTTP {}", status));
        }
        // Check if indexer returned an XML <error> instead of <nzb>
        if let Some(err_msg) = detect_newznab_error(&bytes) {
            return Err(anyhow!("Indexer error: {}", err_msg));
        }
        Ok(bytes.to_vec())
    }

    /// Test an indexer URL + API key via `?t=caps` (and a lightweight search check).
    pub async fn test_indexer(&self, indexer: &IndexerConfig) -> Result<String, String> {
        if indexer.url.trim().is_empty() {
            return Err("Indexer URL is empty".to_string());
        }
        let endpoint = indexer.api_endpoint();
        let start = Instant::now();

        // First test authenticated search with limit=1 so API key validity is truly verified
        let resp = self
            .http
            .get(&endpoint)
            .query(&[
                ("t", "search"),
                ("apikey", indexer.api_key.as_str()),
                ("limit", "1"),
            ])
            .send()
            .await
            .map_err(|e| format!("HTTP error: {}", e))?;

        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("Failed reading body: {}", e))?;
        let ms = start.elapsed().as_millis();

        if !status.is_success() {
            return Err(format!("HTTP {}", status));
        }
        if let Some(err) = detect_newznab_error(&bytes) {
            return Err(err);
        }

        match parse_newznab_rss(&bytes, &indexer.name, &indexer.api_key) {
            Ok(items) => Ok(format!(
                "API OK in {}ms ({} sample item{})",
                ms,
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            )),
            Err(e) => Err(format!("Unexpected response: {}", e)),
        }
    }
}

fn detect_newznab_error(xml: &[u8]) -> Option<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"error" {
                    let mut code = String::new();
                    let mut desc = String::new();
                    for attr in e.attributes().flatten() {
                        match attr.key.local_name().as_ref() {
                            b"code" => {
                                code = attr.unescape_value().map(|v| v.into_owned()).unwrap_or_default();
                            }
                            b"description" => {
                                desc = attr.unescape_value().map(|v| v.into_owned()).unwrap_or_default();
                            }
                            _ => {}
                        }
                    }
                    return Some(format!("code {} — {}", code, desc));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

pub fn parse_newznab_rss(
    xml: &[u8],
    indexer_name: &str,
    api_key: &str,
) -> Result<Vec<SearchResult>> {
    if let Some(err) = detect_newznab_error(xml) {
        return Err(anyhow!("Indexer error: {}", err));
    }

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut results = Vec::new();
    let mut in_item = false;
    let mut current_tag = String::new();

    let mut title = String::new();
    let mut guid = String::new();
    let mut link = String::new();
    let mut enclosure_url = String::new();
    let mut enclosure_len = 0u64;
    let mut category = String::new();
    let mut pub_date: Option<DateTime<Utc>> = None;
    let mut attr_size = 0u64;
    let mut grabs = 0u32;
    let mut files = 0u32;
    let mut password_protected = false;
    let mut group = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                if local == "item" {
                    in_item = true;
                    title.clear();
                    guid.clear();
                    link.clear();
                    enclosure_url.clear();
                    enclosure_len = 0;
                    category.clear();
                    pub_date = None;
                    attr_size = 0;
                    grabs = 0;
                    files = 0;
                    password_protected = false;
                    group.clear();
                } else if in_item {
                    if local == "enclosure" {
                        extract_enclosure(e, &mut enclosure_url, &mut enclosure_len);
                    } else if local == "attr" {
                        extract_newznab_attr(
                            e,
                            &mut attr_size,
                            &mut grabs,
                            &mut files,
                            &mut password_protected,
                            &mut group,
                        );
                    }
                    current_tag = local;
                }
            }
            Ok(Event::Empty(ref e)) if in_item => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                if local == "enclosure" {
                    extract_enclosure(e, &mut enclosure_url, &mut enclosure_len);
                } else if local == "attr" {
                    extract_newznab_attr(
                        e,
                        &mut attr_size,
                        &mut grabs,
                        &mut files,
                        &mut password_protected,
                        &mut group,
                    );
                }
            }
            Ok(Event::Text(ref e)) if in_item => {
                let text = e.unescape().map(|v| v.into_owned()).unwrap_or_default();
                if !text.is_empty() {
                    match current_tag.as_str() {
                        "title" => title.push_str(&text),
                        "guid" => guid.push_str(&text),
                        "link" => link.push_str(&text),
                        "category" => {
                            if category.is_empty() {
                                category = text;
                            }
                        }
                        "pubdate" => {
                            if let Ok(dt) = DateTime::parse_from_rfc2822(&text) {
                                pub_date = Some(dt.with_timezone(&Utc));
                            } else if let Ok(dt) = DateTime::parse_from_rfc3339(&text) {
                                pub_date = Some(dt.with_timezone(&Utc));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(ref e)) if in_item => {
                let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                if current_tag == "title" {
                    title.push_str(&text);
                } else if current_tag == "category" && category.is_empty() {
                    category = text;
                }
            }
            Ok(Event::End(ref e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                if local == "item" && in_item {
                    in_item = false;
                    let nzb_url = if !enclosure_url.is_empty() {
                        enclosure_url.clone()
                    } else {
                        link.clone()
                    };
                    let size = if attr_size > 0 { attr_size } else { enclosure_len };
                    if !title.is_empty() && !nzb_url.is_empty() {
                        results.push(SearchResult {
                            title: title.trim().to_string(),
                            guid: guid.trim().to_string(),
                            nzb_url: nzb_url.trim().to_string(),
                            indexer_name: indexer_name.to_string(),
                            indexer_api_key: api_key.to_string(),
                            category: category.trim().to_string(),
                            size,
                            pub_date,
                            grabs,
                            files,
                            password_protected,
                            group: group.trim().to_string(),
                        });
                    }
                }
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("Failed parsing Newznab XML: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(results)
}

fn extract_enclosure(
    e: &quick_xml::events::BytesStart<'_>,
    url: &mut String,
    length: &mut u64,
) {
    for attr in e.attributes().flatten() {
        match attr.key.local_name().as_ref() {
            b"url" => {
                if let Ok(v) = attr.unescape_value() {
                    *url = v.into_owned();
                }
            }
            b"length" => {
                if let Ok(v) = attr.unescape_value() {
                    if let Ok(n) = v.parse::<u64>() {
                        *length = n;
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_newznab_attr(
    e: &quick_xml::events::BytesStart<'_>,
    size: &mut u64,
    grabs: &mut u32,
    files: &mut u32,
    password_protected: &mut bool,
    group: &mut String,
) {
    let mut name = String::new();
    let mut value = String::new();
    for attr in e.attributes().flatten() {
        match attr.key.local_name().as_ref() {
            b"name" => {
                name = attr
                    .unescape_value()
                    .map(|v| v.to_lowercase())
                    .unwrap_or_default();
            }
            b"value" => {
                value = attr
                    .unescape_value()
                    .map(|v| v.into_owned())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    match name.as_str() {
        "size" => {
            if let Ok(n) = value.parse::<u64>() {
                *size = n;
            }
        }
        "grabs" => {
            if let Ok(n) = value.parse::<u32>() {
                *grabs = n;
            }
        }
        "files" => {
            if let Ok(n) = value.parse::<u32>() {
                *files = n;
            }
        }
        "password" => {
            if value == "1" || value == "2" {
                *password_protected = true;
            }
        }
        "group" => {
            *group = value;
        }
        _ => {}
    }
}

pub fn build_search_params(
    api_key: &str,
    query: &str,
    category_id: &str,
    season: Option<&str>,
    limit: usize,
) -> Vec<(&'static str, String)> {
    let season_trimmed = season.map(str::trim).filter(|s| !s.is_empty());
    let search_type = if season_trimmed.is_some() {
        "tvsearch"
    } else {
        "search"
    };
    let mut params: Vec<(&'static str, String)> = vec![
        ("t", search_type.to_string()),
        ("apikey", api_key.to_string()),
        ("extended", "1".to_string()),
        ("limit", limit.to_string()),
    ];
    if !query.trim().is_empty() {
        params.push(("q", query.trim().to_string()));
    }
    if !category_id.is_empty() {
        params.push(("cat", category_id.to_string()));
    }
    if let Some(s) = season_trimmed {
        params.push(("season", s.to_string()));
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_newznab_rss() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <item>
      <title>Ubuntu.26.10.Desktop.amd64</title>
      <guid>abc123</guid>
      <link>https://indexer.example/getnzb/abc123.nzb</link>
      <pubDate>Thu, 19 Nov 2026 12:00:00 +0000</pubDate>
      <category>PC &gt; ISO</category>
      <enclosure url="https://indexer.example/api?t=get&amp;id=abc123" length="4200000000" type="application/x-nzb"/>
      <newznab:attr name="size" value="4200000000"/>
      <newznab:attr name="grabs" value="142"/>
      <newznab:attr name="files" value="25"/>
      <newznab:attr name="group" value="alt.binaries.linux"/>
    </item>
  </channel>
</rss>"#;

        let items = parse_newznab_rss(xml, "TestIdx", "secret").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Ubuntu.26.10.Desktop.amd64");
        assert_eq!(items[0].size, 4_200_000_000);
        assert_eq!(items[0].grabs, 142);
        assert_eq!(items[0].files, 25);
        assert!(items[0].nzb_url.contains("t=get&id=abc123"));
    }

    #[test]
    fn test_detect_error_xml() {
        let xml = br#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#;
        let err = parse_newznab_rss(xml, "TestIdx", "bad").unwrap_err();
        assert!(err.to_string().contains("Incorrect user credentials"));
    }

    #[test]
    fn test_build_search_params_with_season() {
        let normal = build_search_params("key", "ubuntu", "4000", None, 100);
        assert!(normal.contains(&("t", "search".to_string())));
        assert!(!normal.iter().any(|(k, _)| *k == "season"));

        let packs = build_search_params("key", "South Park", "5000", Some("00"), 100);
        assert!(packs.contains(&("t", "tvsearch".to_string())));
        assert!(packs.contains(&("season", "00".to_string())));
        assert!(packs.contains(&("q", "South Park".to_string())));
        assert!(packs.contains(&("cat", "5000".to_string())));

        let s2 = build_search_params("key", "Severance", "", Some(" 2 "), 50);
        assert!(s2.contains(&("t", "tvsearch".to_string())));
        assert!(s2.contains(&("season", "2".to_string())));
        assert!(!s2.iter().any(|(k, _)| *k == "cat"));
    }
}
