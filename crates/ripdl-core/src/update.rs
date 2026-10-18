//! Update checker: "is there a newer release on GitHub?"
//!
//! One direct HTTPS GET to `api.github.com` whenever a frontend asks for it
//! (GUI startup / Help menu, `ripdl update`). That request carries no
//! identifying data (no cookies, no machine fingerprint — just a generic
//! User-Agent and the endpoint path), and there is no telemetry channel of
//! any kind. ripdl never auto-downloads or self-replaces: the reply is
//! information only and the user decides what to do with it.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::Duration;

use crate::error::{Error, Result};

/// GitHub repo whose Releases drive the check.
pub const RELEASE_REPO: &str = "ShiniM0d0ri/ripdl";
/// Human-visible releases page (the "open page" action targets this).
pub const RELEASES_PAGE: &str = "https://github.com/ShiniM0d0ri/ripdl/releases";

const API_LATEST: &str =
    "https://api.github.com/repos/ShiniM0d0ri/ripdl/releases/latest";

/// The version this binary was built from (workspace `Cargo.toml`; every
/// crate shares it, so the GUI and CLI agree with the daemon).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// What we read back from GitHub's release JSON (subset).
#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    /// Markdown notes; may be long, frontends truncate or drop it.
    #[serde(default)]
    body: Option<String>,
}

/// One check result, as every frontend renders it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Version the running binary reports.
    pub current: String,
    /// Latest tagged release, `v` prefix stripped.
    pub latest: String,
    /// Release page URL for the latest release.
    pub url: String,
    /// ISO-8601 publish timestamp straight from GitHub (may be absent).
    pub published_at: Option<String>,
    /// Markdown release notes (may be absent).
    pub notes: Option<String>,
    /// `latest` is strictly newer than `current`.
    pub update_available: bool,
}

/// Check the pinned repo's latest stable release against the running build.
pub async fn fetch_latest() -> Result<UpdateInfo> {
    fetch_latest_from(API_LATEST, current_version()).await
}

/// Endpoint-injected variant so tests can hit a local mock instead of
/// GitHub, and so the checker never becomes a hidden network call.
pub async fn fetch_latest_from(endpoint: &str, current: &str) -> Result<UpdateInfo> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Update(format!("http client: {e}")))?;

    let resp = client
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "ripdl-updater")
        .send()
        .await
        .map_err(|e| Error::Update(format!("github unreachable: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Update(format!(
            "github api returned {} (tries a rate limit? retry later)",
            resp.status().as_u16()
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| Error::Update(format!("github reply unreadable: {e}")))?;
    let rel: GithubRelease =
        serde_json::from_str(&body).map_err(|e| Error::Update(format!("github reply: {e}")))?;

    let latest = rel.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
    let update_available = compare_versions(&latest, current) == Ordering::Greater;
    Ok(UpdateInfo {
        current: current.to_string(),
        latest,
        url: rel.html_url,
        published_at: rel.published_at,
        notes: rel.body,
        update_available,
    })
}

/// Compare two dotted versions by segment ("0.4.10" > "0.3.1", "0.10" >
/// "0.9", "0.4" == "0.4.0"). Pre-release suffixes order below the plain
/// release ("1.0.0-rc1" < "1.0.0"); junk segments digest as zero so an
/// oddball tag never crashes the float.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a_num, a_pre) = split_prerelease(a.trim());
    let (b_num, b_pre) = split_prerelease(b.trim());
    let ord = cmp_dotted(a_num, b_num);
    if ord != Ordering::Equal {
        return ord;
    }
    match (a_pre.is_empty(), b_pre.is_empty()) {
        (true, true) => Ordering::Equal,
        // Same numbers: the real release outranks any pre-release.
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a_pre.cmp(b_pre),
    }
}

fn split_prerelease(v: &str) -> (&str, &str) {
    match v.split_once('-') {
        Some((n, p)) => (n, p),
        None => (v, ""),
    }
}

fn cmp_dotted(a: &str, b: &str) -> Ordering {
    // Missing segments count as zero: "0.4" == "0.4.0".
    let mut va: Vec<u64> = a.split('.').map(numeric_prefix).collect();
    let mut vb: Vec<u64> = b.split('.').map(numeric_prefix).collect();
    // Trailing zeros never carry meaning; trimming keeps the resize trick
    // honest ("1.0" == "1").
    while va.last() == Some(&0) {
        va.pop();
    }
    while vb.last() == Some(&0) {
        vb.pop();
    }
    let n = va.len().max(vb.len());
    va.resize(n, 0);
    vb.resize(n, 0);
    va.cmp(&vb)
}

/// Leading digit run of one segment, parsed; junk segments are zero.
fn numeric_prefix(seg: &str) -> u64 {
    let digits: String = seg
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn versions_order() {
        assert_eq!(compare_versions("0.4.0", "0.3.1"), Greater);
        assert_eq!(compare_versions("0.3.1", "0.4.0"), Less);
        assert_eq!(compare_versions("0.3.1", "0.3.1"), Equal);
        assert_eq!(compare_versions("0.10", "0.9"), Greater);
        assert_eq!(compare_versions("0.4", "0.4.0"), Equal);
        assert_eq!(compare_versions("0.4", "0.4.1"), Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc1"), Greater);
        assert_eq!(compare_versions("1.0.0-rc2", "1.0.0-rc1"), Greater);
        assert_eq!(compare_versions("1", "1.0"), Equal);
        // Junk never panics and never invents a "newer".
        assert_eq!(compare_versions("garbage", "0"), Equal);
        assert_eq!(compare_versions("garbage", "garbage"), Equal);
    }

    /// Local mock of GitHub's release endpoint: one canned JSON reply.
    #[tokio::test]
    async fn check_against_mock() {
        let body = r#"{
            "tag_name": "v0.4.0",
            "html_url": "https://github.com/ShiniM0d0ri/ripdl/releases/tag/v0.4.0",
            "published_at": "2026-10-20T00:00:00Z",
            "body": "Test notes"
        }"#;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let req = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(req.contains("GET /repos/ShiniM0d0ri/ripdl/releases/latest"));
            assert!(req.contains("ripdl-updater"));
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let info = fetch_latest_from(&format!("http://{addr}/repos/ShiniM0d0ri/ripdl/releases/latest"), "0.3.1")
            .await
            .unwrap();
        server.await.unwrap();
        assert!(info.update_available);
        assert_eq!(info.latest, "0.4.0");
        assert_eq!(info.current, "0.3.1");
        assert_eq!(info.notes.as_deref(), Some("Test notes"));
    }

    /// Same test, bound for the "already latest" path — one listener shot.
    #[tokio::test]
    async fn check_already_latest() {
        let body = r#"{"tag_name":"v0.3.1","html_url":"https://x/0.3.1"}"#;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await.unwrap();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });
        let info = fetch_latest_from(&format!("http://{addr}/latest"), "0.3.1").await.unwrap();
        server.await.unwrap();
        assert!(!info.update_available);
    }
}
