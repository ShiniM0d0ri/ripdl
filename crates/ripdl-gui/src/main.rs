//! ripdl GUI: a thin Tauri shell over the daemon RPC.
//!
//! The window is pure static HTML/CSS/JS (IDM-style task manager) that talks
//! to the local daemon through the commands below. The GUI owns no download
//! logic; every feature already lives in the daemon/engine.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ripdl_core::bridge;
use ripdl_core::categories::categorize;
use ripdl_core::engine::percent_decode_lossy;
use ripdl_core::rpc::{
    BundlePart, Command, DaemonSettings, ExtensionStatus, Reply, Request, Response, TaskInfo,
};
use ripdl_core::task::{TaskKind, TaskState};
use ripdl_core::transport;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

mod single_instance;
mod webview_mem;

/// Set once an in-app update has handed over to the installer. While it is
/// set the GUI must not help: polling would auto-spawn the daemon the
/// installer is trying to close (a windowless daemon makes Windows' Restart
/// Manager stop an interactive install with "unable to close all
/// applications"), and a close request must actually exit instead of
/// minimizing to the tray.
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One-shot RPC round trip against the daemon, with a single
/// spawn-and-retry recovery: cold starts (fresh login after boot) make the
/// user wait for the daemon otherwise.
async fn rpc(cmd: Command) -> Result<Reply, String> {
    if INSTALLING.load(std::sync::atomic::Ordering::Relaxed) {
        // During an install the daemon is expected to be gone.
        return rpc_once(cmd).await;
    }
    match rpc_once(cmd.clone()).await {
        Ok(reply) => Ok(reply),
        Err(_) => {
            let _ = bridge::spawn_daemon_bg();
            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
            rpc_once(cmd).await
        }
    }
}

async fn rpc_once(cmd: Command) -> Result<Reply, String> {
    let mut stream = transport::connect_daemon()
        .await
        .map_err(|e| format!("daemon not running ({e})"))?;
    let req = Request { id: 1, cmd };
    let mut line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let reply = tokio::io::BufReader::new(stream)
        .lines()
        .next_line()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("daemon closed without a reply")?;
    let resp: Response =
        serde_json::from_str(&reply).map_err(|e| format!("bad daemon reply: {e}"))?;
    Ok(resp.reply)
}

fn download_root() -> std::path::PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// File-name part of a URL (query/fragment stripped, percent-decoded).
fn url_basename(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    let name = no_query.rsplit('/').next().unwrap_or("");
    if name.is_empty() {
        url.to_string()
    } else {
        percent_decode_lossy(name)
    }
}

/// Subfolder for a new task. `Some("auto")`/`None` = core's
/// auto-categorization; explicit picks honor the user ("other" = none).
fn category_subdir_for(url: &str, category: Option<&str>) -> Option<&'static str> {
    match category {
        Some("auto") | None => categorize(&url_basename(url)).subdir(),
        Some("video") => Some("Videos"),
        Some("music") => Some("Music"),
        Some("images") => Some("Images"),
        Some("documents") => Some("Documents"),
        Some("archives") => Some("Archives"),
        Some("programs") => Some("Programs"),
        _ => None,
    }
}

#[tauri::command]
async fn list_tasks() -> Result<Vec<TaskInfo>, String> {
    let reply = rpc(Command::List).await?;
    match reply {
        Reply::Tasks { tasks } => Ok(tasks),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Queue a URL. `dest_dir` overrides the location entirely (the Add dialog
/// passes an explicit user choice); otherwise category rules apply.
#[tauri::command]
async fn add_task(
    url: String,
    category: Option<String>,
    dest_dir: Option<String>,
    sequential: Option<bool>,
    only_files: Option<Vec<usize>>,
) -> Result<String, String> {
    let dest = match dest_dir {
        Some(d) if !d.trim().is_empty() => std::path::PathBuf::from(d),
        _ => {
            let mut d = download_root();
            if let Some(sub) = category_subdir_for(&url, category.as_deref()) {
                d.push(sub);
            }
            d
        }
    };
    let _ = tokio::fs::create_dir_all(&dest).await;
    let kind = if ripdl_core::torrent::looks_like_torrent_payload(&url) {
        TaskKind::Torrent
    } else {
        TaskKind::Http
    };
    match rpc(Command::Add {
        url,
        dest_dir: Some(dest),
        headers: Default::default(),
        start_at: None,
        kind,
        sequential: sequential.unwrap_or(false),
        only_files,
        on_duplicate: Default::default(),
    })
    .await?
    {
        Reply::Added { id } => Ok(id),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Pre-add torrent preview for the Add dialog (qB-style file list before
/// queuing). Magnets resolve over the swarm and can take a while.
#[tauri::command]
async fn torrent_preview(payload: String) -> Result<ripdl_core::torrent::TorrentPreviewSnapshot, String> {
    match rpc(Command::TorrentPreview { payload }).await? {
        Reply::TorrentPreview { preview } => Ok(preview),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Queue a video/stream page through the yt-dlp sidecar.
#[tauri::command]
async fn add_video_task(url: String, dest_dir: Option<String>) -> Result<String, String> {
    let dest = match dest_dir {
        Some(d) if !d.trim().is_empty() => std::path::PathBuf::from(d),
        _ => download_root(),
    };
    let _ = tokio::fs::create_dir_all(&dest).await;
    match rpc(Command::Add {
        url,
        dest_dir: Some(dest),
        headers: Default::default(),
        start_at: None,
        kind: TaskKind::Video,
        sequential: false,
        only_files: None,
        on_duplicate: Default::default(),
    })
    .await?
    {
        Reply::Added { id } => Ok(id),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Queue several direct links as **one** bundle task that owns a folder
/// (`<dest>/<name>/<parts...>`). The dialog sends one URL per line; grouping
/// and the folder name are the daemon's call unless the user typed a name.
#[tauri::command]
async fn add_bundle(
    urls: Vec<String>,
    name: Option<String>,
    dest_dir: Option<String>,
) -> Result<String, String> {
    let dest = match dest_dir {
        Some(d) if !d.trim().is_empty() => std::path::PathBuf::from(d),
        _ => download_root(),
    };
    let _ = tokio::fs::create_dir_all(&dest).await;
    let parts: Vec<BundlePart> = urls
        .into_iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .map(|url| BundlePart {
            url,
            filename: None,
            headers: Default::default(),
            state: TaskState::Queued,
            downloaded: 0,
            total: None,
            seq: None,
            error: None,
            attempted: false,
        })
        .collect();
    if parts.is_empty() {
        return Err("no links given".into());
    }
    let name = name.filter(|n| !n.trim().is_empty());
    match rpc(Command::AddBundle {
        name,
        dest_dir: Some(dest),
        parts,
        start_at: None,
    })
    .await?
    {
        Reply::Added { id } => Ok(id),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Append links to an existing bundle ("I forgot part 7"). New links must
/// name parts of that bundle; URLs already in it are skipped. Returns how
/// many parts actually joined.
#[tauri::command]
async fn add_bundle_parts(id: String, urls: Vec<String>) -> Result<usize, String> {
    let parts: Vec<BundlePart> = urls
        .into_iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .map(|url| BundlePart {
            url,
            filename: None,
            headers: Default::default(),
            state: TaskState::Queued,
            downloaded: 0,
            total: None,
            seq: None,
            error: None,
            attempted: false,
        })
        .collect();
    if parts.is_empty() {
        return Err("no links given".into());
    }
    match rpc(Command::AddBundleParts { id, parts }).await? {
        Reply::BundlePartsAdded { added, .. } => Ok(added),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Group a selection without queueing anything: the Add-bundle dialog shows
/// what the links would become, so "12 links" is never a leap of faith.
#[tauri::command]
async fn preview_bundle(urls: Vec<String>) -> Result<Vec<ripdl_core::bundle::Group>, String> {
    let links: Vec<BundlePart> = urls
        .into_iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .map(|url| BundlePart {
            url,
            filename: None,
            headers: Default::default(),
            state: TaskState::Queued,
            downloaded: 0,
            total: None,
            seq: None,
            error: None,
            attempted: false,
        })
        .collect();
    match rpc(Command::PreviewBundle { links }).await? {
        Reply::BundlePreview { groups } => Ok(groups),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

#[tauri::command]
async fn pause_task(id: String) -> Result<(), String> {
    expect_ok(rpc(Command::Pause { id }).await?)
}

#[tauri::command]
async fn resume_task(id: String) -> Result<(), String> {
    expect_ok(rpc(Command::Resume { id }).await?)
}

/// Change a torrent task's per-file selection (`only_files` = None
/// selects everything; works mid-flight and persists across restarts).
#[tauri::command]
async fn update_selection(id: String, only_files: Option<Vec<usize>>) -> Result<(), String> {
    expect_ok(rpc(Command::UpdateTorrentSelection { id, only_files }).await?)
}

/// Remove the queue record; `delete_files` also erases the downloaded
/// file(s) and their sidecars from disk.
#[tauri::command]
async fn remove_task(id: String, delete_files: Option<bool>) -> Result<(), String> {
    expect_ok(
        rpc(Command::Remove {
            id,
            delete_files: delete_files.unwrap_or(false),
        })
        .await?,
    )
}

/// Quit the whole app (File > Exit). Tray "Exit" uses its own handler; this
/// is the webview's entry point.
#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Update check through the daemon.
#[tauri::command]
async fn check_update() -> Result<ripdl_core::update::UpdateInfo, String> {
    match rpc(Command::CheckUpdate).await? {
        Reply::Update { info } => Ok(info),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// One-click upgrade: daemon downloads the verified setup and runs it.
#[tauri::command]
async fn install_update() -> Result<(), String> {
    let out = expect_ok(rpc(Command::UpdateInstall).await?);
    if out.is_ok() {
        // The installer is running now: stop spawning daemons and let the
        // next close request through (see INSTALLING).
        INSTALLING.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    out
}

/// The running build's version (Help > About). Read from the crate rather
/// than tauri.conf.json so it can never drift from the binaries.
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn managed_extension_dir() -> Result<std::path::PathBuf, String> {
    Ok(transport::app_data_dir()
        .map_err(|e| e.to_string())?
        .join("extension"))
}

fn bundled_extension_dir() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let start = exe.parent().ok_or("GUI executable has no parent folder")?;
    for dir in start.ancestors().take(5) {
        let candidate = dir.join("ext");
        if candidate.join("manifest.json").is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("cannot find bundled ext folder beside {}", exe.display()))
}

fn sync_managed_extension() -> Result<ripdl_core::extplug::ManagedCopy, String> {
    ripdl_core::extplug::sync_managed(&bundled_extension_dir()?, &managed_extension_dir()?)
}

#[derive(serde::Serialize)]
struct BrowserExtensionInfo {
    folder: String,
    folder_version: Option<String>,
    bundled_version: String,
    loaded: ExtensionStatus,
    browser: Option<String>,
}

#[derive(Clone, Copy)]
struct ChromiumBrowser {
    name: &'static str,
    extensions_url: &'static str,
}

#[cfg(windows)]
fn default_chromium_browser() -> Option<ChromiumBrowser> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
            "/v",
            "ProgId",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let prog_id = text.split_whitespace().last()?.to_ascii_lowercase();
    if prog_id.starts_with("chromehtml") {
        Some(ChromiumBrowser { name: "Chrome", extensions_url: "chrome://extensions" })
    } else if prog_id.starts_with("msedgehtm") {
        Some(ChromiumBrowser { name: "Edge", extensions_url: "edge://extensions" })
    } else if prog_id.starts_with("bravehtml") {
        Some(ChromiumBrowser { name: "Brave", extensions_url: "brave://extensions" })
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn default_chromium_browser() -> Option<ChromiumBrowser> {
    let output = std::process::Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let desktop_id = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if desktop_id.contains("microsoft-edge") || desktop_id.contains("msedge") {
        Some(ChromiumBrowser { name: "Edge", extensions_url: "edge://extensions" })
    } else if desktop_id.contains("brave") {
        Some(ChromiumBrowser { name: "Brave", extensions_url: "brave://extensions" })
    } else if desktop_id.contains("chrome") || desktop_id.contains("chromium") {
        Some(ChromiumBrowser { name: "Chrome", extensions_url: "chrome://extensions" })
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn default_chromium_browser() -> Option<ChromiumBrowser> {
    None
}

#[tauri::command]
async fn browser_extension_info(dir: String) -> Result<BrowserExtensionInfo, String> {
    let managed = managed_extension_dir()?;
    let bundled_version = ripdl_core::extplug::folder_version(&bundled_extension_dir()?)
        .ok_or("bundled extension has no readable manifest version")?;
    let loaded = match rpc(Command::GetConfig).await? {
        Reply::Config { extension, .. } => extension,
        other => return Err(format!("unexpected daemon reply: {other:?}")),
    };
    let folder = if dir.trim().is_empty() {
        managed
    } else {
        std::path::PathBuf::from(dir.trim())
    };
    Ok(BrowserExtensionInfo {
        folder_version: ripdl_core::extplug::folder_version(&folder),
        folder: folder.to_string_lossy().into_owned(),
        bundled_version,
        loaded,
        browser: default_chromium_browser().map(|b| b.name.to_string()),
    })
}

#[tauri::command]
async fn open_browser_extensions(dir: String, browser: Option<String>) -> Result<String, String> {
    let _ = tauri::async_runtime::spawn_blocking(sync_managed_extension)
        .await
        .map_err(|e| e.to_string())??;
    let browser = match browser.as_deref().unwrap_or("") {
        "" => default_chromium_browser()
            .ok_or("could not detect a supported default browser; select Chrome, Edge, or Brave")?,
        "chrome" => ChromiumBrowser { name: "Chrome", extensions_url: "chrome://extensions" },
        "edge" => ChromiumBrowser { name: "Edge", extensions_url: "edge://extensions" },
        "brave" => ChromiumBrowser { name: "Brave", extensions_url: "brave://extensions" },
        _ => return Err("unsupported browser selection".into()),
    };
    let folder = if dir.trim().is_empty() {
        managed_extension_dir()?
    } else {
        std::path::PathBuf::from(dir.trim())
    };
    let version = ripdl_core::extplug::folder_version(&folder)
        .ok_or_else(|| format!("{} has no readable extension manifest", folder.display()))?;
    open_extensions_page(browser).map_err(|e| format!("could not open {} extensions: {e}", browser.name))?;
    Ok(format!(
        "Opened {}. Extension folder: {} (v{}).",
        browser.name,
        folder.display(),
        version
    ))
}

#[cfg(windows)]
fn open_extensions_page(browser: ChromiumBrowser) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;

    let relative = match browser.name {
        "Chrome" => "Google/Chrome/Application/chrome.exe",
        "Edge" => "Microsoft/Edge/Application/msedge.exe",
        "Brave" => "BraveSoftware/Brave-Browser/Application/brave.exe",
        _ => return Err(std::io::Error::other("unsupported browser")),
    };
    let executable_name = relative.rsplit('/').next().unwrap_or_default();
    let mut candidates = Vec::new();
    for hive in ["HKCU", "HKLM"] {
        let key = format!(r"{hive}\Software\Microsoft\Windows\CurrentVersion\App Paths\{executable_name}");
        if let Ok(output) = std::process::Command::new("reg")
            .args(["query", &key, "/ve"])
            .creation_flags(0x0800_0000)
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(value) = text.lines().find_map(|line| line.split_once("REG_SZ").map(|(_, value)| value.trim().trim_matches('"'))) {
                    candidates.push(std::path::PathBuf::from(value));
                }
            }
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            candidates.push(std::path::PathBuf::from(root).join(relative));
        }
    }
    let executable = candidates.into_iter().find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} executable not found; open {} manually", browser.name, browser.extensions_url)))?;
    std::process::Command::new(executable)
        .arg(browser.extensions_url)
        .creation_flags(0x0800_0000)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn open_extensions_page(browser: ChromiumBrowser) -> std::io::Result<()> {
    let programs: &[&str] = match browser.name {
        "Chrome" => &["google-chrome", "chromium", "chromium-browser"],
        "Edge" => &["microsoft-edge", "microsoft-edge-stable"],
        "Brave" => &["brave-browser", "brave"],
        _ => return Err(std::io::Error::other("unsupported browser")),
    };
    for program in programs {
        match std::process::Command::new(program).arg(browser.extensions_url).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} executable not found; open {} manually", browser.name, browser.extensions_url)))
}

/// Settings line: the extension folder's manifest version beside this app's.
#[derive(serde::Serialize)]
struct ExtFolder {
    /// `None` when the folder has no readable `manifest.json`.
    version: Option<String>,
    app: String,
    /// `same`, `older` (the folder predates this app) or `newer`; `None` without a version.
    skew: Option<&'static str>,
}

#[tauri::command]
fn extension_folder(dir: String) -> ExtFolder {
    let dir = dir.trim();
    let version = if dir.is_empty() {
        None
    } else {
        ripdl_core::extplug::folder_version(std::path::Path::new(dir))
    };
    let app = app_version();
    let skew = version
        .as_deref()
        .map(|v| match ripdl_core::update::compare_versions(v, &app) {
            std::cmp::Ordering::Equal => "same",
            std::cmp::Ordering::Less => "older",
            std::cmp::Ordering::Greater => "newer",
        });
    ExtFolder { version, app, skew }
}

/// Open a URL in the user's browser (update banner, Help menu).
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("not an https url: {url}"));
    }
    open_with_default(&url).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open_with_default(url: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // cmd start is the reliable way to hand a URL to the default handler
    // without an extra dependency; the empty arg eats quoting hazards.
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn open_with_default(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let prog = "xdg-open";
    std::process::Command::new(prog).arg(url).spawn().map(|_| ())
}

/// Native directory picker for the Add dialog's "Save to" field.
#[tauri::command]
async fn pick_dir() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose download folder")
            .pick_folder()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Native file picker for "Add Torrent" (.torrent files).
#[tauri::command]
async fn pick_file() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose torrent file")
            .add_filter("Torrent files", &["torrent"])
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Native file picker for the Settings "Install hoster plugin" flow.
#[tauri::command]
async fn pick_js_file() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose hoster plugin script")
            .add_filter("Script plugins", &["js"])
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Copy a user script into the selected extension's `hosts/local/` folder.
/// The app-managed copy is the default; `extension_dir` selects a manual copy.
#[tauri::command]
async fn install_plugin(path: String) -> Result<String, String> {
    let managed = managed_extension_dir()?;
    let override_dir = match rpc(Command::GetConfig).await? {
        Reply::Config { settings, .. } => settings.extension_dir,
        other => return Err(format!("unexpected daemon reply: {other:?}")),
    };
    let extension_dir = override_dir.unwrap_or_else(|| managed.clone());
    if extension_dir == managed {
        tauri::async_runtime::spawn_blocking(sync_managed_extension)
            .await
            .map_err(|e| e.to_string())??;
    }
    let content = std::fs::read(&path).map_err(|e| format!("{}: {e}", path))?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ripdl_core::extplug::install(std::path::Path::new(&extension_dir), &name, &content)
}

/// Native file picker for a LAN send (any file).
#[tauri::command]
async fn pick_any_file() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose a file to send")
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// LAN send/receive (thin wrappers over the daemon's drop box)
// ---------------------------------------------------------------------------

#[tauri::command]
async fn lan_info() -> Result<ripdl_core::rpc::LanInfo, String> {
    match rpc(Command::LanInfo).await? {
        Reply::Lan { info } => Ok(info),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

#[tauri::command]
async fn lan_peers() -> Result<Vec<ripdl_core::lan::LanPeer>, String> {
    match rpc(Command::LanPeers).await? {
        Reply::LanPeers { peers } => Ok(peers),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

#[tauri::command]
async fn lan_send(url: String, code: String, path: String) -> Result<String, String> {
    let cmd = Command::LanSend {
        url,
        code,
        path: std::path::PathBuf::from(path),
    };
    match rpc(cmd).await? {
        Reply::LanSend { id } => Ok(id),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

#[tauri::command]
async fn lan_sends() -> Result<Vec<ripdl_core::rpc::LanSendInfo>, String> {
    match rpc(Command::LanSends).await? {
        Reply::LanSends { sends } => Ok(sends),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

#[tauri::command]
async fn lan_forget(id: String) -> Result<(), String> {
    expect_ok(rpc(Command::LanSendForget { id }).await?)
}

#[tauri::command]
async fn lan_new_code() -> Result<ripdl_core::rpc::LanInfo, String> {
    match rpc(Command::LanNewCode).await? {
        Reply::Lan { info } => Ok(info),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Toggle receiving / trusted mode by patching the live settings, then
/// return the state the daemon actually reached (a port clash shows up as
/// `running: false` instead of a silent lie).
#[tauri::command]
async fn lan_configure(
    enabled: Option<bool>,
    open: Option<bool>,
) -> Result<ripdl_core::rpc::LanInfo, String> {
    let mut settings = match rpc(Command::GetConfig).await? {
        Reply::Config { settings, .. } => settings,
        other => return Err(format!("unexpected daemon reply: {other:?}")),
    };
    if let Some(v) = enabled {
        settings.lan_enabled = Some(v);
    }
    if let Some(v) = open {
        settings.lan_open = Some(v);
    }
    match rpc(Command::SetConfig { settings }).await? {
        Reply::Config { .. } => {}
        Reply::Error { message } => return Err(message),
        other => return Err(format!("unexpected daemon reply: {other:?}")),
    }
    lan_info().await
}

/// Files/peers/trackers snapshot for one torrent task.
#[tauri::command]
async fn torrent_details(id: String) -> Result<ripdl_core::torrent::TorrentDetailsSnapshot, String> {
    match rpc(Command::Details { id }).await? {
        Reply::TorrentDetails { details } => Ok(details),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Engine view for an HTTP/video task: output file, resume identity and the
/// byte-range segment map (what the details dialog renders).
#[tauri::command]
async fn task_details(id: String) -> Result<ripdl_core::rpc::TaskDetailsSnapshot, String> {
    match rpc(Command::Details { id }).await? {
        Reply::TaskDetails { details } => Ok(details),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

#[derive(serde::Serialize)]
struct RefreshOutcome {
    /// Durable bytes kept on disk.
    kept: u64,
    total: Option<u64>,
}

/// Point a paused/failed download at a fresh URL (expired signed link) and
/// resume it: the daemon keeps the bytes already downloaded. `part` targets
/// one part of a bundle (by its part number) instead of the whole task.
#[tauri::command]
async fn refresh_url(id: String, url: String, part: Option<u32>) -> Result<RefreshOutcome, String> {
    match rpc(Command::RefreshUrl {
        id,
        url,
        resume: true,
        part,
    })
    .await?
    {
        Reply::Refreshed { kept, total, .. } => Ok(RefreshOutcome { kept, total }),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Open the task's folder (or reveal the completed file in it).
#[tauri::command]
async fn open_path(path: String, reveal: Option<String>) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    tokio::task::spawn_blocking(move || open_in_explorer(p, reveal))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(windows)]
fn open_in_explorer(p: std::path::PathBuf, reveal: Option<String>) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    match reveal {
        // /select,<file> reveals the exact file inside its folder.
        Some(f) => {
            let full = p.join(&f);
            if full.exists() {
                cmd.arg(format!("/select,{}", full.display()));
            } else {
                cmd.arg(&p);
            }
        }
        None => {
            cmd.arg(&p);
        }
    }
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn open_in_explorer(p: std::path::PathBuf, _reveal: Option<String>) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(p)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn download_root_path() -> String {
    download_root().to_string_lossy().into_owned()
}

/// Current daemon settings (config.json contents).
#[tauri::command]
async fn get_config() -> Result<DaemonSettings, String> {
    match rpc(Command::GetConfig).await? {
        Reply::Config { settings, .. } => Ok(settings),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Live HTTP-bond uplinks for the status-bar chips. Empty while bonding is
/// off (the chips stay hidden; the Settings sentence is the "off" story).
#[tauri::command]
async fn bond_links() -> Result<Vec<ripdl_core::rpc::BondLink>, String> {
    match rpc(Command::GetConfig).await? {
        Reply::Config { bond_links, .. } => Ok(bond_links),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// One internet uplink bonding would spread connections across. Shown in
/// Settings so a single-uplink machine can see the toggle is a no-op.
#[derive(serde::Serialize)]
struct UplinkView {
    name: String,
    addr: String,
    default: bool,
}

/// Uplinks detected right now (same discovery the daemon uses for torrent
/// bonding; HTTP tasks read it per download).
#[tauri::command]
fn bond_nics() -> Vec<UplinkView> {
    ripdl_core::engine::bonded_nics()
        .into_iter()
        .map(|n| UplinkView {
            name: n.name,
            addr: n.addr.to_string(),
            default: n.is_default_route,
        })
        .collect()
}

/// Live torrent-bonding state. The setting alone can lie: the shared session
/// fixes `bind_ips` when it is created, so a toggle applies only after a
/// daemon restart. `text` is the same sentence `ripdl config` prints.
#[derive(serde::Serialize)]
struct BondState {
    text: String,
    active: bool,
}

#[tauri::command]
async fn bond_state() -> Result<BondState, String> {
    match rpc(Command::GetConfig).await? {
        Reply::Config {
            settings,
            torrent_session,
            torrent_bind,
            torrent_rates,
            torrent_unreachable,
            ..
        } => {
            let bind: Vec<std::net::IpAddr> = torrent_bind
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect();
            let mut text = ripdl_core::engine::describe_torrent_bond(
                settings.bond_interfaces(),
                torrent_session,
                &bind,
            );
            // Measured per-uplink throughput: the reason a slow tether gets
            // skipped ("Wi-Fi 7.0 MB/s, Ethernet 2 idle").
            if torrent_session && !torrent_rates.is_empty() {
                let nics = ripdl_core::engine::bonded_nics();
                // Fold v4+v6 of the same interface into one number.
                let mut merged: Vec<(String, f64)> = Vec::new();
                for (ip, rate) in &torrent_rates {
                    let addr: Option<std::net::IpAddr> = ip.parse().ok();
                    let name = addr
                        .and_then(|a| {
                            nics.iter()
                                .find(|n| {
                                    std::net::IpAddr::V4(n.addr) == a
                                        || n.v6.map(std::net::IpAddr::V6) == Some(a)
                                })
                                .map(|n| n.name.clone())
                        })
                        .unwrap_or_else(|| ip.clone());
                    match merged.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, r)) => *r += *rate,
                        None => merged.push((name, *rate)),
                    }
                }
                let parts: Vec<String> = merged
                    .iter()
                    .map(|(name, rate)| {
                        if *rate < 1024.0 {
                            format!("{name} idle")
                        } else {
                            format!("{name} {:.1} MB/s", rate / 1_048_576.0)
                        }
                    })
                    .collect();
                text = format!("{text} \u{2014} {}", parts.join(" \u{00b7} "));
            }
            if !torrent_unreachable.is_empty() {
                text = format!(
                    "{text} \u{2014} skipped {} (no working path)",
                    torrent_unreachable.join(", ")
                );
            }
            Ok(BondState {
                active: torrent_session && !bind.is_empty(),
                text,
            })
        }
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Stop the daemon and let the bridge spawn a fresh one. Needed for settings
/// fixed at torrent-session creation (NIC bonding); active transfers resume
/// from their records/sidecars.
#[tauri::command]
async fn restart_daemon() -> Result<(), String> {
    // A stop cancels runners, and the queue may record them as Paused (it
    // races the process exit). Remember what was moving and replay resume on
    // the fresh daemon so "active transfers resume" is deterministic.
    let running: Vec<String> = match rpc_once(Command::List).await {
        Ok(Reply::Tasks { tasks }) => tasks
            .into_iter()
            .filter(|t| t.state == ripdl_core::task::TaskState::Running)
            .map(|t| t.id)
            .collect(),
        _ => Vec::new(),
    };
    let _ = rpc_once(Command::Shutdown).await;
    for _ in 0..60 {
        if transport::connect_daemon().await.is_err() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    rpc(Command::List).await?;
    for id in running {
        // Best effort per task: HTTP resumes from its sidecar, a torrent
        // re-attaches to the session (and re-checks existing data first).
        let _ = rpc(Command::Resume { id }).await;
    }
    Ok(())
}

/// Replace the daemon settings (validation happens daemon-side; the reply
/// carries the normalized values). Also flips the tray preference live.
#[tauri::command]
async fn set_config(
    settings: DaemonSettings,
    state: tauri::State<'_, GuiPrefs>,
) -> Result<DaemonSettings, String> {
    let saved = match rpc(Command::SetConfig { settings }).await? {
        Reply::Config { settings, .. } => settings,
        other => Err(format!("unexpected daemon reply: {other:?}"))?,
    };
    state
        .hide_to_tray
        .store(saved.hide_to_tray.unwrap_or(true), std::sync::atomic::Ordering::Relaxed);
    Ok(saved)
}

/// Live tray preference read by the close handler (avoid an async config
/// roundtrip inside the window event). Managed; defaults true and is
/// synced from the daemon settings at startup / on save.
struct GuiPrefs {
    hide_to_tray: std::sync::atomic::AtomicBool,
}

impl Default for GuiPrefs {
    fn default() -> Self {
        // Hide-to-tray on close until the user's config says otherwise.
        Self { hide_to_tray: std::sync::atomic::AtomicBool::new(true) }
    }
}

fn expect_ok(reply: Reply) -> Result<(), String> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error { message } => Err(message),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Bring the task window forward, waking the webview first: it is suspended
/// whenever the window has been hidden to the tray.
fn reveal(window: &tauri::WebviewWindow) {
    webview_mem::resume(window);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

fn main() {
    // One GUI per machine: a second launch asks the running window to come
    // forward and exits before any window or tray icon is created.
    let guard = match single_instance::claim() {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => return,
        Err(e) => {
            eprintln!("ripdl: single-instance guard unavailable ({e}); starting anyway");
            None
        }
    };

    tauri::Builder::default()
        .manage(GuiPrefs::default())
        .invoke_handler(tauri::generate_handler![
            list_tasks,
            add_task,
            torrent_preview,
            add_video_task,
            add_bundle,
            add_bundle_parts,
            preview_bundle,
            pause_task,
            resume_task,
            update_selection,
            remove_task,
            pick_dir,
            pick_file,
            pick_js_file,
            pick_any_file,
            install_plugin,
            lan_info,
            lan_peers,
            lan_send,
            lan_sends,
            lan_forget,
            lan_new_code,
            lan_configure,
            torrent_details,
            task_details,
            refresh_url,
            open_path,
            download_root_path,
            get_config,
            bond_links,
            set_config,
            bond_nics,
            bond_state,
            restart_daemon,
            check_update,
            install_update,
            app_version,
            extension_folder,
            browser_extension_info,
            open_browser_extensions,
            open_url,
            exit_app
        ])
        .setup(move |app| {
            use tauri::Manager;

            if let Err(e) = sync_managed_extension() {
                eprintln!("ripdl: cannot sync browser extension: {e}");
            }

            // Serve "show" requests from later launches.
            if let Some(guard) = guard {
                let handle = app.handle().clone();
                guard.spawn_listener(move || {
                    if let Some(w) = handle.get_webview_window("main") {
                        reveal(&w);
                    }
                });
            }

            // Sync the tray preference from the daemon settings.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(Reply::Config { settings, .. }) = rpc(Command::GetConfig).await {
                    if let Some(prefs) = handle.try_state::<GuiPrefs>() {
                        prefs.hide_to_tray.store(
                            settings.hide_to_tray.unwrap_or(true),
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
            });

            // Tray icon: left click shows the window; menu has Show / Exit.
            let show_item =
                tauri::menu::MenuItem::with_id(app, "tray-show", "Show ripdl", true, None::<&str>)?;
            let quit_item =
                tauri::menu::MenuItem::with_id(app, "tray-quit", "Exit", true, None::<&str>)?;
            let tray_menu = tauri::menu::Menu::with_items(app, &[&show_item, &quit_item])?;
            let mut tray = tauri::tray::TrayIconBuilder::with_id("ripdl-tray")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "tray-show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            reveal(&w);
                        }
                    }
                    "tray-quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    // Left click = show the window (like IDM/FDM).
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            reveal(&w);
                        }
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            use tauri::Manager;
            // Close minimizes to the tray by default (the daemon keeps
            // running either way; Quit lives in the tray menu).
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let hide = window
                    .app_handle()
                    .try_state::<GuiPrefs>()
                    .is_some_and(|p| p.hide_to_tray.load(std::sync::atomic::Ordering::Relaxed))
                    // An installer is closing us: exiting is the point.
                    && !INSTALLING.load(std::sync::atomic::Ordering::Relaxed);
                if hide && window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                    // Hiding a WebView2 frees nothing on its own; suspend it
                    // while it is off screen (see webview_mem.rs).
                    if let Some(w) = window.app_handle().get_webview_window("main") {
                        webview_mem::suspend(&w);
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running ripdl gui");
}
