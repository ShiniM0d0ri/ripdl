use reqwest::header::{ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, ETAG};
use reqwest::{Client, StatusCode};

use crate::error::{Error, Result};

/// What the engine learns about a URL before downloading.
#[derive(Debug, Clone)]
pub struct PlanInfo {
    pub url: reqwest::Url,
    pub total: Option<u64>,
    pub accept_ranges: bool,
    pub filename: String,
    pub etag: Option<String>,
}

/// Probe a URL with HEAD (falling back to a 1-byte ranged GET) to learn
/// size, range support, and a filename before downloading anything.
pub async fn probe(client: &Client, url: &str) -> Result<PlanInfo> {
    let parsed: reqwest::Url =
        url.parse().map_err(|_| Error::InvalidUrl(url.to_string()))?;

    let resp = match client.head(parsed.clone()).send().await {
        Ok(r) if r.status().is_success() => r,
        // Some servers reject HEAD; ask for the first byte instead.
        _ => {
            client
                .get(parsed.clone())
                .header(reqwest::header::RANGE, "bytes=0-0")
                .send()
                .await?
        }
    };

    let status = resp.status();
    if !(status.is_success() || status == StatusCode::PARTIAL_CONTENT) {
        return Err(Error::InvalidUrl(format!("server returned {status}")));
    }

    let accept_ranges = resp
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("bytes"))
        .unwrap_or(false)
        || resp.headers().get(CONTENT_RANGE).is_some();

    let total = total_from(&resp);
    let filename = filename_from(&resp)
        .or_else(|| filename_from_url(&parsed))
        .unwrap_or_else(|| "download.bin".to_string());
    let etag = resp
        .headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    Ok(PlanInfo {
        url: parsed,
        total,
        accept_ranges,
        filename,
        etag,
    })
}

fn total_from(resp: &reqwest::Response) -> Option<u64> {
    let headers = resp.headers();
    if let Some(len) = headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()) {
        if let Ok(n) = len.parse::<u64>() {
            if n > 1 {
                return Some(n);
            }
        }
    }
    // Ranged probe: "bytes 0-0/12345"
    if let Some(cr) = headers.get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
        if let Some(total) = cr.rsplit('/').next() {
            if let Ok(n) = total.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

fn filename_from(resp: &reqwest::Response) -> Option<String> {
    let cd = resp.headers().get(CONTENT_DISPOSITION)?.to_str().ok()?;
    let lower = cd.to_ascii_lowercase();
    let idx = lower.find("filename=")?;
    let raw = cd[idx + 9..].trim_start().trim_start_matches('"');
    let end = raw
        .find('"')
        .or_else(|| raw.find(';'))
        .unwrap_or(raw.len());
    let name = raw[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn filename_from_url(url: &reqwest::Url) -> Option<String> {
    url.path_segments()
        .and_then(|mut segs| segs.next_back())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_from_url_takes_last_segment() {
        let url: reqwest::Url = "https://example.com/files/big-file.zip".parse().unwrap();
        assert_eq!(filename_from_url(&url).as_deref(), Some("big-file.zip"));
    }
}
