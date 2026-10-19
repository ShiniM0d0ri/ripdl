//! Update checker + verified installer-fetch.
//!
//! Checks are one plain `api.github.com` GET per request (`CheckUpdate`
//! RPC, `ripdl update`, GUI startup) — no cookies, no fingerprint, no
//! telemetry. `UpdateInstall` additionally downloads the Windows
//! setup.exe into `<data_dir>/update/`, verifies it against the release's
//! `SHA256SUMS.txt`, and launches Inno Setup to replace the running app.
//! Nothing runs without a click or the opted-in `update_auto_install`.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};

/// GitHub repo whose Releases drive the check.
pub const RELEASE_REPO: &str = "ShiniM0d0ri/ripdl";
/// Human-visible releases page.
pub const RELEASES_PAGE: &str = "https://github.com/ShiniM0d0ri/ripdl/releases";

const API_LATEST: &str =
    "https://api.github.com/repos/ShiniM0d0ri/ripdl/releases/latest";

/// Installers land in `<data_dir>/update/`.
pub fn update_dir(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("update")
}

/// Tampered-asset failure mode is "refuse", not "fill the disk".
const MAX_SETUP_BYTES: u64 = 256 * 1024 * 1024;

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// One release asset (the fields we use).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateAsset {
    #[serde(default)]
    pub name: String,
    /// GitHub API asset URL (`api.github.com/.../assets/<id>`). This returns
    /// JSON metadata, **not** the file bytes, so it must never be downloaded
    /// directly.
    #[serde(default)]
    pub url: String,
    /// Direct download URL (`browser_download_url`). This is what downloads
    /// must use.
    #[serde(default)]
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

impl UpdateAsset {
    fn is_windows_setup(&self) -> bool {
        self.name.ends_with("-x86_64-windows-setup.exe")
    }
    fn is_sums(&self) -> bool {
        self.name.eq_ignore_ascii_case("SHA256SUMS.txt")
    }
    /// The URL to fetch bytes from: the web URL when the API supplied one
    /// (GitHub always does), else the raw `url` (local mocks).
    fn download_url(&self) -> &str {
        if self.browser_download_url.is_empty() {
            &self.url
        } else {
            &self.browser_download_url
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<UpdateAsset>,
}

/// One check result, as every frontend renders it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current: String,
    /// Latest tagged release, `v` prefix stripped.
    pub latest: String,
    pub url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// `latest` is strictly newer than `current`.
    pub update_available: bool,
    /// Windows installer asset, when attached.
    #[serde(default)]
    pub setup: Option<UpdateAsset>,
    /// Checksum source for `setup`, when attached.
    #[serde(default)]
    pub sha256: Option<UpdateAsset>,
}

/// Latest stable release of the pinned repo vs the running build.
pub async fn fetch_latest() -> Result<UpdateInfo> {
    fetch_latest_from(API_LATEST, current_version()).await
}

/// Endpoint-injected so tests can hit a local mock.
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
            "github api returned {} (rate limited? retry later)",
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
    let setup = rel.assets.iter().find(|a| a.is_windows_setup()).cloned();
    let sha256 = rel.assets.iter().find(|a| a.is_sums()).cloned();
    Ok(UpdateInfo {
        current: current.to_string(),
        latest,
        url: rel.html_url,
        published_at: rel.published_at,
        notes: rel.body,
        update_available,
        setup,
        sha256,
    })
}

/// Dotted compare: "0.4.10" > "0.3.1", "0.10" > "0.9", "0.4" == "0.4.0".
/// Pre-releases order below the plain release ("1.0.0-rc1" < "1.0.0");
/// junk segments digest as zero.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a_num, a_pre) = split_prerelease(a.trim());
    let (b_num, b_pre) = split_prerelease(b.trim());
    let ord = cmp_dotted(a_num, b_num);
    if ord != Ordering::Equal {
        return ord;
    }
    match (a_pre.is_empty(), b_pre.is_empty()) {
        (true, true) => Ordering::Equal,
        // Same numbers: the release outranks any pre-release.
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
    // Missing segments are zero ("0.4" == "0.4.0"); trailing zeros are
    // dropped first ("1.0" == "1").
    let mut va: Vec<u64> = a.split('.').map(numeric_prefix).collect();
    let mut vb: Vec<u64> = b.split('.').map(numeric_prefix).collect();
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

/// Parse a GNU sha256sum file ("digest  filename" per line; binary mode
/// writes `digest *filename`).
fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if let Some((d, f)) = l.split_once("  ") {
                Some((d, f.trim().trim_start_matches('*')))
            } else if let Some((d, f)) = l.split_once(" *") {
                Some((d, f))
            } else {
                None
            }
        })
        .map(|(d, f)| (f.trim().to_string(), d.trim().to_ascii_lowercase()))
        .filter(|(f, d)| !f.is_empty() && d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit()))
        .collect()
}

/// `Vec<u8>` digest → lowercase hex.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Download the Windows setup into `dest_dir`. When a SHA256SUMS asset is
/// given, the filename must be listed there and the digest must match.
/// A previously downloaded copy (same name + size, the directory only
/// retains hash-verified files) is reused without re-downloading.
pub async fn download_setup(
    dest_dir: &std::path::Path,
    setup: &UpdateAsset,
    sums: Option<&UpdateAsset>,
) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    // Path-safety: the name is joined into dest_dir unescaped.
    let suspicious = setup.name.is_empty()
        || setup.name.contains("..")
        || setup.name.contains('/')
        || setup.name.contains('\\')
        || !setup.is_windows_setup();
    if suspicious {
        return Err(Error::Update(format!("suspicious asset name: {:?}", setup.name)));
    }

    std::fs::create_dir_all(dest_dir).map_err(Error::Io)?;
    let dest = dest_dir.join(&setup.name);

    // Fast reuse path: exact name+size match on an existing file.
    if setup.size > 0 {
        if let Ok(md) = std::fs::metadata(&dest) {
            if md.len() == setup.size {
                return Ok(dest);
            }
        }
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Update(format!("http client: {e}")))?;

    // Expected digest from SHA256SUMS.txt. Missing sums for a filename the
    // sums file was expected to cover = refuse, never install blind.
    let mut expected = None;
    if let Some(sums) = sums {
        let text = client
            .get(sums.download_url())
            .header(reqwest::header::USER_AGENT, "ripdl-updater")
            .send()
            .await
            .map_err(|e| Error::Update(format!("sums unreachable: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Update(format!("sums unreachable: {e}")))?
            .text()
            .await
            .map_err(|e| Error::Update(format!("sums unreadable: {e}")))?;
        expected = parse_sums(&text).get(&setup.name).cloned();
        if expected.is_none() {
            return Err(Error::Update(format!(
                "{} not listed in {}; refusing unverified install",
                setup.name, sums.name
            )));
        }
    }

    // Same size but wrong content is possible for a size-only reuse miss:
    // verify bytes against the digest when one is known.
    if let (Some(want), Ok(bytes)) = (&expected, std::fs::read(&dest)) {
        if hex(&Sha256::digest(&bytes)) == *want {
            return Ok(dest);
        }
    }

    let mut resp = client
        .get(setup.download_url())
        .header(reqwest::header::USER_AGENT, "ripdl-updater")
        .send()
        .await
        .map_err(|e| Error::Update(format!("download failed: {e}")))?
        .error_for_status()
        .map_err(|e| Error::Update(format!("download failed: {e}")))?;
    if let Some(len) = resp.content_length() {
        if len > MAX_SETUP_BYTES {
            return Err(Error::Update("setup.exe impossibly large".to_string()));
        }
    }

    // Atomic: stream to `.part`, then rename, so a half download can not
    // be mistaken for a complete installer by the size-based reuse path.
    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await.map_err(Error::Io)?;
    let mut received: u64 = 0;
    let mut hasher = Sha256::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Update(format!("download failed: {e}")))?
    {
        received += chunk.len() as u64;
        if received > MAX_SETUP_BYTES {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(Error::Update("setup.exe impossibly large".to_string()));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(Error::Io)?;
    }
    file.flush().await.map_err(Error::Io)?;
    drop(file);

    if let Some(want) = &expected {
        let got = hex(&hasher.finalize());
        if got != *want {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(Error::Update(format!(
                "checksum mismatch for {}: got {got}, want {want}",
                setup.name
            )));
        }
    }
    if setup.size > 0 && received != setup.size {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(Error::Update(format!(
            "size mismatch: got {received}, expected {}",
            setup.size
        )));
    }
    tokio::fs::rename(&tmp, &dest).await.map_err(Error::Io)?;
    Ok(dest)
}

/// Whether this process already has administrator rights. Silent
/// (unattended) installs only run when true - otherwise Windows would pop a
/// UAC prompt with nobody to answer it.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// Run the installer elevated through `ShellExecuteW("runas")`, which is what
/// surfaces the UAC prompt. Error 1223 = the user declined it.
#[cfg(windows)]
fn shell_execute_runas(path: &std::path::Path, args: &str) -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let verb = wide("runas");
    let file = wide(&path.to_string_lossy());
    let params = wide(args);
    // SW_SHOWNORMAL = 1; ShellExecute returns > 32 on success.
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            1,
        )
    } as isize;
    if code <= 32 {
        let why = if code == 1223 {
            "elevation was cancelled".to_string()
        } else {
            format!("Windows refused to elevate (code {code})")
        };
        return Err(Error::Update(format!(
            "installer needs administrator rights: {why}"
        )));
    }
    Ok(())
}

/// Launch the downloaded installer (Inno Setup stops the running app and
/// replaces it in place itself). `silent` = `/VERYSILENT /NORESTART`, the
/// unattended auto-install path; a non-elevated process re-launches through
/// ShellExecute so the UAC prompt appears instead of failing with error 740.
pub fn run_installer(path: &std::path::Path, silent: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let args = if silent { "/VERYSILENT /NORESTART" } else { "" };
        if !is_elevated() {
            return shell_execute_runas(path, args);
        }
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = std::process::Command::new(path);
        if silent {
            cmd.args(["/VERYSILENT", "/NORESTART"]);
        }
        cmd.creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| Error::Update(format!("failed to start installer: {e}")))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, silent);
        Err(Error::Update(
            "automatic install is Windows-only; grab the release from the page".to_string(),
        ))
    }
}

/// Remove installers older than a week and loose `.part` files.
pub fn prune_update_dir(update_dir: &std::path::Path) {
    let Some(entries) = std::fs::read_dir(update_dir).ok() else {
        return;
    };
    let week = std::time::Duration::from_secs(7 * 24 * 3600);
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".part") {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if let Ok(md) = entry.metadata() {
            if md
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > week)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
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

    #[test]
    fn asset_picking() {
        let assets = vec![
            UpdateAsset {
                name: "ripdl-v0.4.0-x86_64-linux.tar.gz".into(),
                url: "http://api/x/l".into(),
                browser_download_url: "http://x/l".into(),
                size: 1,
            },
            UpdateAsset {
                name: "ripdl-v0.4.0-x86_64-windows.zip".into(),
                url: "http://api/x/p".into(),
                browser_download_url: "http://x/p".into(),
                size: 2,
            },
            UpdateAsset {
                name: "ripdl-v0.4.0-x86_64-windows-setup.exe".into(),
                url: "http://api/x/s".into(),
                browser_download_url: "http://x/s".into(),
                size: 3,
            },
            UpdateAsset {
                name: "SHA256SUMS.txt".into(),
                url: "http://api/x/h".into(),
                browser_download_url: "http://x/h".into(),
                size: 4,
            },
        ];
        let setup = assets.iter().find(|a| a.is_windows_setup()).unwrap();
        let sums = assets.iter().find(|a| a.is_sums()).unwrap();
        assert_eq!(setup.download_url(), "http://x/s"); // installer, not the portable zip
        assert_eq!(sums.download_url(), "http://x/h");
        // API asset URLs return JSON metadata; never download from them.
        assert_ne!(setup.download_url(), setup.url);
    }

    /// Regression: GitHub's asset object carries both `url` (API JSON) and
    /// `browser_download_url` (bytes). The updater shipped reading `url`,
    /// so every real install failed at the checksum gate.
    #[test]
    fn asset_prefers_browser_download_url() {
        let json = r#"{
            "name": "ripdl-v0.4.0-x86_64-windows-setup.exe",
            "url": "https://api.github.com/repos/o/r/releases/assets/42",
            "browser_download_url": "https://github.com/o/r/releases/download/v0.4.0/ripdl-v0.4.0-x86_64-windows-setup.exe",
            "size": 1234
        }"#;
        let a: UpdateAsset = serde_json::from_str(json).unwrap();
        assert_eq!(a.size, 1234);
        assert!(a.download_url().starts_with("https://github.com/"));
        assert!(!a.download_url().contains("api.github.com"));
    }

    #[test]
    fn sums_parse() {
        let text = format!(            "{}  ripdl-v0.4.0-x86_64-windows-setup.exe\n\
             bad-line-without-hash\n\
             {}  portabl.*zip\n",
            "a".repeat(64),
            "b".repeat(64),
        );
        let m = parse_sums(&text);
        assert!(m.contains_key(&format!("ripdl-v0.4.0-x86_64-windows-setup.exe")));
        assert!(m.contains_key("portabl.*zip"));
        assert_eq!(m.len(), 2); // junk line filtered (wrong digest length)
    }

    #[tokio::test]
    async fn download_verifies_and_reuses() {
        use sha2::{Digest, Sha256};

        let body = "SETUP-BYTES-0123456789ABCDEF".as_bytes().to_vec();
        let sha = hex(&Sha256::digest(&body));
        let name = "ripdl-v0.4.0-x86_64-windows-setup.exe".to_string();
        let sums = format!("{sha}  {name}\n");
        let (body_len, sums_len) = (body.len(), sums.len());
        let body_s = String::from_utf8(body.clone()).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Sums first, then the setup file.
            for (snippet, payload) in
                [("/SHA256SUMS.txt", sums), ("windows-setup.exe", body_s)]
            {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap();
                assert!(std::str::from_utf8(&buf[..n]).unwrap().contains(snippet));
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        // GitHub-shaped assets: `url` is the API endpoint (JSON), the web URL
        // is what must be fetched. Point `url` at the mock too so the test
        // stays offline while still exercising the browser_download_url path.
        let setup = UpdateAsset {
            name: name.clone(),
            url: format!("http://{addr}/api-json"),
            browser_download_url: format!("http://{addr}/{name}"),
            size: body_len as u64,
        };
        let sums_asset = UpdateAsset {
            name: "SHA256SUMS.txt".to_string(),
            url: format!("http://{addr}/api-json"),
            browser_download_url: format!("http://{addr}/SHA256SUMS.txt"),
            size: sums_len as u64,
        };
        let dir = std::env::temp_dir().join(format!("ripdl-update-test-{}", std::process::id()));
        let got = download_setup(&dir, &setup, Some(&sums_asset)).await.unwrap();
        server.await.unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), body);

        // Reuse: on-disk file matches, no second run against the mock.
        let again = download_setup(&dir, &setup, Some(&sums_asset)).await.unwrap();
        assert_eq!(again, got);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
