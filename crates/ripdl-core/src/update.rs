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
    /// Shape check for the path-safety gate; release picking is exact, see [`pick_assets`].
    fn is_windows_setup(&self) -> bool {
        self.name.ends_with("-x86_64-windows-setup.exe")
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
    /// Windows installer for `latest`: exact name, served from this repo's
    /// release for the tag (see [`pick_assets`]). `None` when absent.
    #[serde(default)]
    pub setup: Option<UpdateAsset>,
    /// `SHA256SUMS.txt` of the same release; required to install `setup`.
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
    let (setup, sha256) = pick_assets(&latest, &rel.assets);
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

/// Name of the Windows installer that release `version` must carry.
pub fn setup_name(version: &str) -> String {
    format!("ripdl-v{version}-x86_64-windows-setup.exe")
}

/// The checksum file every release carries next to its installer.
pub const SUMS_NAME: &str = "SHA256SUMS.txt";

/// The installer and checksum of release `version`: exact names, and only
/// when served from this repo's release page for that tag. A lookalike name,
/// another host, or an older build's installer is never offered as an update.
pub fn pick_assets(version: &str, assets: &[UpdateAsset]) -> (Option<UpdateAsset>, Option<UpdateAsset>) {
    let pick = |name: &str| {
        let served_from = format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{name}");
        assets
            .iter()
            .find(|a| a.name == name && a.browser_download_url == served_from)
            .cloned()
    };
    (pick(&setup_name(version)), pick(SUMS_NAME))
}

/// The installer and checksum an install needs, both or nothing. An install
/// never runs without the digest that proves the bytes.
pub fn plan_install(info: &UpdateInfo) -> Result<(&UpdateAsset, &UpdateAsset)> {
    let setup = info.setup.as_ref().ok_or_else(|| {
        Error::Update(format!("release v{} has no verified Windows installer", info.latest))
    })?;
    let sums = info.sha256.as_ref().ok_or_else(|| {
        Error::Update(format!(
            "release v{} has no {}; refusing an unverified install",
            info.latest, SUMS_NAME
        ))
    })?;
    Ok((setup, sums))
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

/// An installer whose bytes matched the release's checksum.
#[derive(Debug, Clone)]
pub struct VerifiedSetup {
    pub path: PathBuf,
    /// Lowercase hex SHA-256 the bytes were checked against.
    pub sha256: String,
}

/// Fetch the release installer into `dest_dir`, verified against the release's
/// `SHA256SUMS.txt`. The checksum is read on every call, and a file is reused
/// only when its bytes hash to that checksum right now (never by name or
/// size). A download that fails the check is deleted, not kept.
pub async fn download_setup(
    dest_dir: &std::path::Path,
    setup: &UpdateAsset,
    sums: &UpdateAsset,
) -> Result<VerifiedSetup> {
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

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Update(format!("http client: {e}")))?;

    // The digest comes first, and it is required: an installer that the
    // checksum file does not list is never fetched.
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
    let expected = parse_sums(&text).get(&setup.name).cloned().ok_or_else(|| {
        Error::Update(format!(
            "{} not listed in {}; refusing unverified install",
            setup.name, sums.name
        ))
    })?;

    // A copy already on disk is reused only if its bytes hash to the digest.
    if let Ok(file) = std::fs::File::open(&dest) {
        if hash_reader(file).ok().as_deref() == Some(expected.as_str()) {
            return Ok(VerifiedSetup { path: dest, sha256: expected });
        }
    }

    let tmp = dest.with_extension("part");
    let got = match stream_to(&client, setup, &tmp).await {
        Ok(got) => got,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    if got != expected {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(Error::Update(format!(
            "checksum mismatch for {}: got {got}, want {expected}",
            setup.name
        )));
    }
    tokio::fs::rename(&tmp, &dest).await.map_err(Error::Io)?;
    Ok(VerifiedSetup { path: dest, sha256: expected })
}

/// Streams the installer to `tmp`, enforcing the size cap and the listed
/// size. Returns the SHA-256 of the bytes written.
async fn stream_to(
    client: &reqwest::Client,
    setup: &UpdateAsset,
    tmp: &std::path::Path,
) -> Result<String> {
    use sha2::{Digest, Sha256};

    let mut resp = client
        .get(setup.download_url())
        .header(reqwest::header::USER_AGENT, "ripdl-updater")
        .send()
        .await
        .map_err(|e| Error::Update(format!("download failed: {e}")))?
        .error_for_status()
        .map_err(|e| Error::Update(format!("download failed: {e}")))?;
    if resp.content_length().is_some_and(|len| len > MAX_SETUP_BYTES) {
        return Err(Error::Update("setup.exe impossibly large".to_string()));
    }
    let mut file = tokio::fs::File::create(tmp).await.map_err(Error::Io)?;
    let mut received: u64 = 0;
    let mut hasher = Sha256::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Update(format!("download failed: {e}")))?
    {
        received += chunk.len() as u64;
        if received > MAX_SETUP_BYTES {
            return Err(Error::Update("setup.exe impossibly large".to_string()));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(Error::Io)?;
    }
    file.flush().await.map_err(Error::Io)?;
    drop(file);
    if setup.size > 0 && received != setup.size {
        return Err(Error::Update(format!(
            "size mismatch: got {received}, expected {}",
            setup.size
        )));
    }
    Ok(hex(&hasher.finalize()))
}

/// SHA-256 of everything `reader` yields, as lowercase hex.
fn hash_reader(mut reader: impl std::io::Read) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Opens an installer for launch and checks that it still holds `sha256`.
/// On Windows the handle shares read access only, so nothing can replace the
/// file while it is open: keep the returned handle until the installer has
/// started.
pub fn open_verified(path: &std::path::Path, sha256: &str) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ (0x1) and nothing else: no writer can open the file.
        opts.share_mode(0x0000_0001);
    }
    let file = opts.open(path).map_err(Error::Io)?;
    let got = hash_reader(&file).map_err(Error::Io)?;
    if got != sha256 {
        return Err(Error::Update(format!(
            "installer changed on disk (sha256 {got}, want {sha256}); refusing to run it"
        )));
    }
    Ok(file)
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

/// Run the installer through the shell so Windows raises the UAC prompt. The
/// verb is deliberately NULL (the default "open"), *not* "runas": the setup's
/// own manifest already requires admin, and letting the shell elevate keeps
/// Setup aware of the original (non-elevated) user - which is exactly what
/// makes its `runasoriginaluser` [Run] entry relaunch the GUI unelevated.
/// "runas" would hand Setup an already-elevated token instead (the relaunched
/// app then inherits admin). Error 1223/5 = the user declined the prompt.
#[cfg(windows)]
fn shell_execute_elevated(path: &std::path::Path, args: &str) -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let file = wide(&path.to_string_lossy());
    let params = wide(args);
    // SW_SHOWNORMAL = 1; ShellExecute returns > 32 on success.
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            1,
        )
    } as isize;
    if code <= 32 {
        let why = if code == 1223 || code == 5 {
            "the Windows elevation prompt was cancelled".to_string()
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
/// replaces it in place itself). A user-clicked "Download & Install" uses
/// `/SILENT` (progress window only - UAC is the single confirmation);
/// the unattended boot path uses `/VERYSILENT` (no window at all). A
/// non-elevated process re-launches through the shell so the UAC prompt
/// appears instead of failing with error 740.
pub fn run_installer(path: &std::path::Path, silent: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let args = if silent {
            "/VERYSILENT /NORESTART"
        } else {
            "/SILENT /NORESTART"
        };
        if !is_elevated() {
            return shell_execute_elevated(path, args);
        }
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = std::process::Command::new(path);
        cmd.args(args.split_whitespace());
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
        let served = |name: &str| format!("https://github.com/ShiniM0d0ri/ripdl/releases/download/v0.4.0/{name}");
        let asset = |name: &str| UpdateAsset {
            name: name.into(),
            url: "https://api.github.com/repos/ShiniM0d0ri/ripdl/releases/assets/1".into(),
            browser_download_url: served(name),
            size: 1,
        };
        let assets = vec![
            asset("ripdl-v0.4.0-x86_64-linux.tar.gz"),
            asset("ripdl-v0.4.0-x86_64-windows.zip"),
            asset("ripdl-v0.4.0-x86_64-windows-setup.exe"),
            asset("SHA256SUMS.txt"),
        ];
        let (setup, sums) = pick_assets("0.4.0", &assets);
        // The installer, not the portable zip; never the API JSON URL.
        let setup = setup.unwrap();
        assert_eq!(setup.name, "ripdl-v0.4.0-x86_64-windows-setup.exe");
        assert_eq!(setup.download_url(), served(&setup.name));
        assert_eq!(sums.unwrap().name, SUMS_NAME);
    }

    /// Lookalikes, foreign hosts and older builds are never offered.
    #[test]
    fn pick_rejects_lookalikes_and_foreign_hosts() {
        let served = |name: &str| format!("https://github.com/ShiniM0d0ri/ripdl/releases/download/v0.4.0/{name}");
        let asset = |name: &str, from: &str| UpdateAsset {
            name: name.into(),
            url: "https://api.github.com/x".into(),
            browser_download_url: from.into(),
            size: 1,
        };
        // Right name, foreign host.
        let foreign = vec![
            asset("ripdl-v0.4.0-x86_64-windows-setup.exe", "https://evil.example/ripdl-v0.4.0-x86_64-windows-setup.exe"),
            asset("SHA256SUMS.txt", &served("SHA256SUMS.txt")),
        ];
        let (setup, sums) = pick_assets("0.4.0", &foreign);
        assert!(setup.is_none());
        assert!(sums.is_some());
        // Lookalike suffix, an older build's installer, a renamed checksum file.
        let lookalikes = vec![
            asset("evil-x86_64-windows-setup.exe", &served("evil-x86_64-windows-setup.exe")),
            asset("ripdl-v0.3.9-x86_64-windows-setup.exe", &served("ripdl-v0.3.9-x86_64-windows-setup.exe")),
            asset("sha256sums.txt", &served("sha256sums.txt")),
        ];
        let (setup, sums) = pick_assets("0.4.0", &lookalikes);
        assert!(setup.is_none());
        assert!(sums.is_none());
    }

    #[test]
    fn plan_install_needs_both_assets() {
        let asset = |name: &str| UpdateAsset {
            name: name.into(),
            browser_download_url: format!("https://github.com/ShiniM0d0ri/ripdl/releases/download/v0.4.0/{name}"),
            ..Default::default()
        };
        let mut info = UpdateInfo {
            current: "0.3.1".into(),
            latest: "0.4.0".into(),
            url: String::new(),
            published_at: None,
            notes: None,
            update_available: true,
            setup: Some(asset(&setup_name("0.4.0"))),
            sha256: None,
        };
        assert!(plan_install(&info).is_err(), "no checksum, no install");
        info.sha256 = Some(asset(SUMS_NAME));
        let (setup, sums) = plan_install(&info).unwrap();
        assert_eq!(setup.name, "ripdl-v0.4.0-x86_64-windows-setup.exe");
        assert_eq!(sums.name, SUMS_NAME);
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

    const SETUP_NAME: &str = "ripdl-v0.4.0-x86_64-windows-setup.exe";

    /// Loopback server that answers GET by exact path and records each path.
    struct Mock {
        addr: std::net::SocketAddr,
        hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Mock {
        async fn start(routes: Vec<(String, Vec<u8>)>) -> Mock {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = hits.clone();
            let task = tokio::spawn(async move {
                while let Ok((mut sock, _)) = listener.accept().await {
                    let mut buf = [0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                    seen.lock().unwrap().push(path.clone());
                    let body = routes.iter().find(|(p, _)| *p == path).map(|(_, b)| b.clone());
                    let head = match &body {
                        Some(b) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string(),
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    if let Some(b) = body {
                        let _ = sock.write_all(&b).await;
                    }
                }
            });
            Mock { addr, hits, task }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.addr)
        }

        fn paths(&self) -> Vec<String> {
            self.hits.lock().unwrap().clone()
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// An asset as a release lists it, served by `mock`.
    fn asset_at(mock: &Mock, name: &str, size: u64) -> UpdateAsset {
        UpdateAsset {
            name: name.to_string(),
            url: mock.url(&format!("/api/{name}")),
            browser_download_url: mock.url(&format!("/{name}")),
            size,
        }
    }

    /// A SHA256SUMS.txt that lists `digest` for the installer.
    fn sums_listing(digest: &str) -> Vec<u8> {
        format!("{digest}  {SETUP_NAME}\n").into_bytes()
    }

    #[tokio::test]
    async fn download_verifies_and_reuses() {
        use sha2::{Digest, Sha256};
        let body = b"SETUP-BYTES-0123456789ABCDEF".to_vec();
        let sha = hex(&Sha256::digest(&body));
        let sums = sums_listing(&sha);
        let mock = Mock::start(vec![
            ("/SHA256SUMS.txt".into(), sums.clone()),
            (format!("/{SETUP_NAME}"), body.clone()),
        ])
        .await;
        let setup = asset_at(&mock, SETUP_NAME, body.len() as u64);
        let sums_asset = asset_at(&mock, SUMS_NAME, sums.len() as u64);
        let dir = std::env::temp_dir().join(format!("ripdl-update-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let got = download_setup(&dir, &setup, &sums_asset).await.unwrap();
        assert_eq!(got.sha256, sha);
        assert_eq!(std::fs::read(&got.path).unwrap(), body);
        assert_eq!(
            mock.paths(),
            vec!["/SHA256SUMS.txt".to_string(), format!("/{SETUP_NAME}")]
        );

        // Reuse: the checksum is read again, the installer is not fetched.
        let again = download_setup(&dir, &setup, &sums_asset).await.unwrap();
        assert_eq!(again.path, got.path);
        assert_eq!(mock.paths().len(), 3);
        assert_eq!(mock.paths()[2], "/SHA256SUMS.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: reuse used to trust name + size. A same-size file with
    /// other bytes must be fetched again, never launched.
    #[tokio::test]
    async fn same_size_tampered_copy_is_not_reused() {
        use sha2::{Digest, Sha256};
        let body = b"GOOD-INSTALLER-BYTES".to_vec();
        let sums = sums_listing(&hex(&Sha256::digest(&body)));
        let mock = Mock::start(vec![
            ("/SHA256SUMS.txt".into(), sums.clone()),
            (format!("/{SETUP_NAME}"), body.clone()),
        ])
        .await;
        let setup = asset_at(&mock, SETUP_NAME, body.len() as u64);
        let sums_asset = asset_at(&mock, SUMS_NAME, sums.len() as u64);
        let dir = std::env::temp_dir().join(format!("ripdl-update-tamper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Same name and size as the release, different bytes.
        std::fs::write(dir.join(SETUP_NAME), vec![b'X'; body.len()]).unwrap();

        let got = download_setup(&dir, &setup, &sums_asset).await.unwrap();
        assert_eq!(std::fs::read(&got.path).unwrap(), body);
        assert!(
            mock.paths().contains(&format!("/{SETUP_NAME}")),
            "a tampered copy must be fetched again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn checksum_mismatch_leaves_nothing_behind() {
        let served = b"NOT-WHAT-THE-SUMS-SAY".to_vec();
        let sums = sums_listing(&"0".repeat(64));
        let mock = Mock::start(vec![
            ("/SHA256SUMS.txt".into(), sums.clone()),
            (format!("/{SETUP_NAME}"), served.clone()),
        ])
        .await;
        let setup = asset_at(&mock, SETUP_NAME, served.len() as u64);
        let sums_asset = asset_at(&mock, SUMS_NAME, sums.len() as u64);
        let dir = std::env::temp_dir().join(format!("ripdl-update-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let err = download_setup(&dir, &setup, &sums_asset).await.unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(!dir.join(SETUP_NAME).exists());
        assert!(!dir.join("ripdl-v0.4.0-x86_64-windows-setup.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unlisted_installer_is_never_fetched() {
        let sums = format!("{}  ripdl-v0.4.0-x86_64-windows.zip\n", "a".repeat(64)).into_bytes();
        let mock = Mock::start(vec![
            ("/SHA256SUMS.txt".into(), sums.clone()),
            (format!("/{SETUP_NAME}"), b"x".to_vec()),
        ])
        .await;
        let setup = asset_at(&mock, SETUP_NAME, 1);
        let sums_asset = asset_at(&mock, SUMS_NAME, sums.len() as u64);
        let dir = std::env::temp_dir().join(format!("ripdl-update-unlisted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let err = download_setup(&dir, &setup, &sums_asset).await.unwrap_err();
        assert!(err.to_string().contains("not listed"), "{err}");
        assert_eq!(mock.paths(), vec!["/SHA256SUMS.txt".to_string()]);
    }

    /// The launch re-check: right bytes open, changed bytes are refused, and
    /// on Windows the open handle blocks any writer.
    #[test]
    fn open_verified_checks_the_bytes_and_holds_the_file() {
        use sha2::{Digest, Sha256};
        let dir = std::env::temp_dir().join(format!("ripdl-open-verified-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETUP_NAME);
        std::fs::write(&path, b"INSTALLER").unwrap();
        let sha = hex(&Sha256::digest(b"INSTALLER"));

        assert!(open_verified(&path, &"0".repeat(64)).is_err());
        let held = open_verified(&path, &sha).unwrap();
        #[cfg(windows)]
        assert!(
            std::fs::OpenOptions::new().write(true).open(&path).is_err(),
            "a verified installer must not be writable while it is held"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
