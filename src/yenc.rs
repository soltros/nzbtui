use anyhow::{anyhow, Result};
use crc32fast::Hasher;

#[derive(Debug, Clone)]
pub struct DecodedYenc {
    pub filename: String,
    pub total_size: u64,
    /// 0-based byte offset where this part begins in the target file.
    pub offset: u64,
    pub data: Vec<u8>,
    #[allow(dead_code)]
    pub expected_crc32: Option<u32>,
    #[allow(dead_code)]
    pub actual_crc32: u32,
    pub crc_valid: bool,
}

/// Decode a raw NNTP article body encoded with yEnc (single-part or multi-part).
/// Expects dot-unstuffed bytes from the NNTP BODY command.
pub fn decode(raw: &[u8]) -> Result<DecodedYenc> {
    let mut pos = 0usize;
    let len = raw.len();

    // 1. Locate `=ybegin ` header
    let ybegin_line = loop {
        if pos >= len {
            return Err(anyhow!("No =ybegin header found in article"));
        }
        let (line, next_pos) = read_line(raw, pos);
        pos = next_pos;
        if line.starts_with(b"=ybegin ") {
            break line;
        }
    };

    let ybegin_str = String::from_utf8_lossy(ybegin_line);
    let total_size = parse_kv_u64(&ybegin_str, "size")
        .ok_or_else(|| anyhow!("Missing size in =ybegin header"))?;
    let filename = parse_name(&ybegin_str).unwrap_or_else(|| "unknown.bin".to_string());
    let is_multipart = parse_kv_u64(&ybegin_str, "part").is_some();

    // 2. If multipart, locate `=ypart ` header
    let mut offset = 0u64;
    if is_multipart {
        let ypart_line = loop {
            if pos >= len {
                return Err(anyhow!("Multipart yEnc missing =ypart header"));
            }
            let (line, next_pos) = read_line(raw, pos);
            pos = next_pos;
            if line.starts_with(b"=ypart ") {
                break line;
            }
        };
        let ypart_str = String::from_utf8_lossy(ypart_line);
        let begin = parse_kv_u64(&ypart_str, "begin")
            .ok_or_else(|| anyhow!("Missing begin in =ypart header"))?;
        offset = begin.saturating_sub(1);
    }

    // 3. Decode payload lines until `=yend `
    let mut output = Vec::with_capacity(len.saturating_sub(pos));
    let mut expected_crc32: Option<u32> = None;

    while pos < len {
        let (line, next_pos) = read_line(raw, pos);
        pos = next_pos;

        if line.starts_with(b"=yend ") {
            let yend_str = String::from_utf8_lossy(line);
            expected_crc32 = if is_multipart {
                parse_kv_hex(&yend_str, "pcrc32").or_else(|| parse_kv_hex(&yend_str, "crc32"))
            } else {
                parse_kv_hex(&yend_str, "crc32").or_else(|| parse_kv_hex(&yend_str, "pcrc32"))
            };
            break;
        }

        let mut i = 0;
        let line_len = line.len();
        while i < line_len {
            let b = line[i];
            if b == b'=' {
                i += 1;
                if i < line_len {
                    output.push(line[i].wrapping_sub(64).wrapping_sub(42));
                }
            } else {
                output.push(b.wrapping_sub(42));
            }
            i += 1;
        }
    }

    let mut hasher = Hasher::new();
    hasher.update(&output);
    let actual_crc32 = hasher.finalize();
    let crc_valid = match expected_crc32 {
        Some(exp) => exp == actual_crc32,
        None => true,
    };

    Ok(DecodedYenc {
        filename,
        total_size,
        offset,
        data: output,
        expected_crc32,
        actual_crc32,
        crc_valid,
    })
}

fn read_line(buf: &[u8], start: usize) -> (&[u8], usize) {
    let mut end = start;
    while end < buf.len() && buf[end] != b'\n' {
        end += 1;
    }
    let next = if end < buf.len() { end + 1 } else { end };
    let line_end = if end > start && buf[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    (&buf[start..line_end], next)
}

fn parse_kv_u64(line: &str, key: &str) -> Option<u64> {
    let needle = format!("{}=", key);
    for token in line.split_whitespace() {
        if let Some(val) = token.strip_prefix(&needle) {
            if let Ok(n) = val.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

fn parse_kv_hex(line: &str, key: &str) -> Option<u32> {
    let needle = format!("{}=", key);
    for token in line.split_whitespace() {
        if let Some(val) = token.strip_prefix(&needle) {
            if let Ok(n) = u32::from_str_radix(val.trim(), 16) {
                return Some(n);
            }
        }
    }
    None
}

fn parse_name(line: &str) -> Option<String> {
    let idx = line.find("name=")?;
    let name = line[idx + 5..].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Helper used in unit tests to encode data in yEnc format.
#[cfg(test)]
pub fn encode_single_part(filename: &str, data: &[u8]) -> Vec<u8> {
    let mut hasher = Hasher::new();
    hasher.update(data);
    let crc = hasher.finalize();

    let mut out = format!("=ybegin line=128 size={} name={}\r\n", data.len(), filename).into_bytes();
    let mut col = 0;
    for &b in data {
        let enc = b.wrapping_add(42);
        if matches!(enc, 0x00 | 0x0A | 0x0D | b'=') {
            out.push(b'=');
            out.push(enc.wrapping_add(64));
            col += 2;
        } else {
            out.push(enc);
            col += 1;
        }
        if col >= 128 {
            out.extend_from_slice(b"\r\n");
            col = 0;
        }
    }
    if col > 0 {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("=yend size={} crc32={:08x}\r\n", data.len(), crc).as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yenc_single_part_roundtrip() {
        let payload: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        let encoded = encode_single_part("test file.bin", &payload);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.filename, "test file.bin");
        assert_eq!(decoded.total_size, 1024);
        assert_eq!(decoded.offset, 0);
        assert!(decoded.crc_valid);
        assert_eq!(decoded.data, payload);
    }

    #[test]
    fn test_yenc_multipart_offset_and_pcrc() {
        let part_data = b"Hello Usenet World!";
        let mut hasher = Hasher::new();
        hasher.update(part_data);
        let pcrc = hasher.finalize();

        let mut encoded = format!(
            "=ybegin part=2 total=5 line=128 size=1000 name=archive.rar\r\n=ypart begin=101 end=119\r\n"
        )
        .into_bytes();
        for &b in part_data {
            let enc = b.wrapping_add(42);
            if matches!(enc, 0x00 | 0x0A | 0x0D | b'=') {
                encoded.push(b'=');
                encoded.push(enc.wrapping_add(64));
            } else {
                encoded.push(enc);
            }
        }
        encoded.extend_from_slice(
            format!("\r\n=yend size={} part=2 pcrc32={:08x}\r\n", part_data.len(), pcrc).as_bytes(),
        );

        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.filename, "archive.rar");
        assert_eq!(decoded.total_size, 1000);
        assert_eq!(decoded.offset, 100);
        assert!(decoded.crc_valid);
        assert_eq!(decoded.data, part_data);
    }
}
