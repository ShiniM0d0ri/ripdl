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
///
/// Transient answers (408/429/5xx) and transport errors are retried a few
/// times: a host that just asked parallel ranges to slow down must not make
/// a fresh task fail before it starts. Each attempt is short; the download's
/// own backoff takes over once the plan is known.
pub async fn probe(client: &Client, url: &str) -> Result<PlanInfo> {
    let parsed: reqwest::Url =
        url.parse().map_err(|_| Error::InvalidUrl(url.to_string()))?;

    let mut last: Option<Error> = None;
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500 << (attempt - 1))).await;
        }
        let resp = match client.head(parsed.clone()).send().await {
            Ok(r) if r.status().is_success() => Ok(r),
            // Some servers reject HEAD; ask for the first byte instead.
            _ => {
                client
                    .get(parsed.clone())
                    .header(reqwest::header::RANGE, "bytes=0-0")
                    .send()
                    .await
            }
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                last = Some(e.into());
                continue;
            }
        };
        let status = resp.status();
        if !(status.is_success() || status == StatusCode::PARTIAL_CONTENT) {
            let err = Error::Server(format!("server returned {status}"));
            if !super::retry::retryable_status(status.as_u16()) {
                return Err(err);
            }
            last = Some(err);
            continue;
        }

        let mut accept_ranges = resp
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("bytes"))
            .unwrap_or(false)
            || resp.headers().get(CONTENT_RANGE).is_some();

        let mut total = total_from(&resp);

        // Some frontends answer HEAD without either range signal even when
        // ranged GETs work (a Cloudflare-proxied host observed live: HEAD has
        // no Accept-Ranges and no Content-Length, while Range: bytes=0-0
        // answers 206 with the true total). Never trust the header check
        // alone: when HEAD left a question open, ask two bytes and look.
        // A failed follow-up is tolerated - HEAD is a sufficient plan.
        if status == StatusCode::OK && (!accept_ranges || total.is_none()) {
            match client
                .get(parsed.clone())
                .header(reqwest::header::RANGE, "bytes=0-0")
                .send()
                .await
            {
                Ok(r) => {
                    accept_ranges = accept_ranges
                        || r.status() == StatusCode::PARTIAL_CONTENT
                        || r.headers().get(CONTENT_RANGE).is_some();
                    if total.is_none() {
                        total = total_from(&r);
                    }
                }
                Err(_) => {}
            }
        }
        let filename = filename_from(&resp)
            .or_else(|| filename_from_url(&parsed))
            .unwrap_or_else(|| "download.bin".to_string());
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        return Ok(PlanInfo {
            url: parsed,
            total,
            accept_ranges,
            filename,
            etag,
        });
    }
    Err(last.unwrap_or_else(|| Error::Server("probe failed".into())))
}

fn total_from(resp: &reqwest::Response) -> Option<u64> {
    let headers = resp.headers();
    // A 206 is a ranged answer: its Content-Length is the slice's length
    // ("Content-Length: 2" for a bytes=0-0 probe), never the file total. The
    // real total lives in Content-Range ("bytes 0-0/2097152000").
    if resp.status() != StatusCode::PARTIAL_CONTENT {
        if let Some(len) = headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()) {
            if let Ok(n) = len.parse::<u64>() {
                if n > 1 {
                    return Some(n);
                }
            }
        }
    }
    // Ranged probe: "bytes 0-0/12345"
    if let Some(cr) = headers.get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
        if let Some(total) = cr.rsplit('/').next() {
            // 0 (or a parse-failed jumble) must not become a plan total: the
            // planner derives `end: total - 1` and 0u64 - 1 underflows.
            if let Ok(n) = total.parse::<u64>() {
                // 0 (weird server, empty file) must not become a plan total:
                // the planner derives `end: total - 1` and 0u64 - 1 underflows.
                if n > 0 {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn filename_from(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(CONTENT_DISPOSITION)?
        .to_str()
        .ok()
        .and_then(filename_from_content_disposition)
}

/// Parse `Content-Disposition` into a safe single path segment.
///
/// Handles `filename*=UTF-8''percent%20encoded` (RFC 5987; wins when
/// present), `filename="quoted"` and bare `filename=name`. The result is
/// reduced to its basename and rejected when empty or a dot path, so the
/// caller's URL/`download.bin` fallbacks stay in charge.
pub(crate) fn filename_from_content_disposition(cd: &str) -> Option<String> {
    let mut star: Option<String> = None;
    let mut plain: Option<String> = None;
    for param in cd.split(';') {
        let param = param.trim();
        let lower = param.to_ascii_lowercase();
        // Slicing both strings at the same offset is safe: ASCII-lowercasing
        // preserves multibyte boundaries.
        if let Some(rest) = lower.strip_prefix("filename*=") {
            let raw = &param[param.len() - rest.len()..];
            let value = match raw.split_once('\'') {
                // charset'lang'percent-encoded
                Some((_, after1)) => match after1.split_once('\'') {
                    Some((_, value)) => Some(value),
                    None => None,
                },
                None => None,
            }
            .unwrap_or(raw);
            let decoded = percent_decode_lossy(value.trim().trim_matches('"'));
            if !decoded.is_empty() {
                star = Some(decoded);
            }
        } else if let Some(rest) = lower.strip_prefix("filename=") {
            let raw = &param[param.len() - rest.len()..];
            let name = raw.trim().trim_matches('"');
            if !name.is_empty() {
                plain = Some(name.to_string());
            }
        }
    }

    let chosen: String = star.or(plain)?;
    let base = chosen
        .split(['/', '\\'])
        .next_back()
        .unwrap_or("")
        .trim();
    (!base.is_empty() && base != "." && base != "..").then(|| base.to_string())
}

/// Decode `%XX` sequences; invalid ones are kept literally, bytes that don't
/// form UTF-8 are replaced. (`+` is not decoded — that is form encoding.)
pub fn percent_decode_lossy(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(hex) = bytes.get(i + 1..i + 3) {
                if let Some(decoded) =
                    std::str::from_utf8(hex).ok().and_then(|s| u8::from_str_radix(s, 16).ok())
                {
                    out.push(decoded);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

    #[test]
    fn cd_plain_quoted_and_bare() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"report.pdf\"").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename=report.html").as_deref(),
            Some("report.html")
        );
    }

    #[test]
    fn cd_rfc5987_wins_and_decodes() {
        // filename* beats plain filename; percent-encoded UTF-8 survives.
        assert_eq!(
            filename_from_content_disposition(
                "attachment; filename=\"fallback.bin\"; filename*=UTF-8''r%C3%A9sum%C3%A9%20v2.xlsx"
            )
            .as_deref(),
            Some("résumé v2.xlsx")
        );
    }

    #[test]
    fn cd_reduces_to_safe_basename() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"dir/evil/file.iso\"").as_deref(),
            Some("file.iso")
        );
        // Traversal collapses to a harmless basename: we only ever write
        // inside the chosen directory.
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"..\\..\\evil.exe\"").as_deref(),
            Some("evil.exe")
        );
        assert_eq!(filename_from_content_disposition("attachment; filename=\"\"").as_deref(), None);
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\".\"").as_deref(),
            None
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"..\"").as_deref(),
            None
        );
        // Tunneling / missing header values must not yield anything either.
        assert_eq!(filename_from_content_disposition("attachment").as_deref(), None);
    }
}
