//! ripdl GUI: a thin Tauri shell over the daemon RPC.
//!
//! The window is pure static HTML/CSS/JS (IDM-style task manager) that talks
//! to the local daemon through the commands below. The GUI owns no download
//! logic; every feature already lives in the daemon/engine.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ripdl_core::bridge;
use ripdl_core::categories::categorize;
use ripdl_core::engine::percent_decode_lossy;
use ripdl_core::rpc::{Command, DaemonSettings, Reply, Request, Response, TaskInfo};
use ripdl_core::transport;
use ripdl_core::task::TaskKind;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// One-shot RPC round trip against the daemon, with a single
/// spawn-and-retry recovery: cold starts (fresh login after boot) make the
/// user wait for the daemon otherwise.
async fn rpc(cmd: Command) -> Result<Reply, String> {
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
async fn add_task(url: String, category: Option<String>, dest_dir: Option<String>, sequential: Option<bool>) -> Result<String, String> {
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
        url: url.clone(),
        dest_dir: Some(dest),
        headers: Default::default(),
        start_at: None,
        kind,
        sequential: sequential.unwrap_or(false),
    })
    .await?
    {
        Reply::Added { id } => Ok(id),
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
    })
    .await?
    {
        Reply::Added { id } => Ok(id),
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

/// Files/peers/trackers snapshot for one torrent task.
#[tauri::command]
async fn torrent_details(id: String) -> Result<ripdl_core::torrent::TorrentDetailsSnapshot, String> {
    match rpc(Command::Details { id }).await? {
        Reply::TorrentDetails { details } => Ok(details),
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
        Reply::Config { settings } => Ok(settings),
        other => Err(format!("unexpected daemon reply: {other:?}")),
    }
}

/// Replace the daemon settings (validation happens daemon-side; the reply
/// carries the normalized values). Also flips the tray preference live.
#[tauri::command]
async fn set_config(
    settings: DaemonSettings,
    state: tauri::State<'_, GuiPrefs>,
) -> Result<DaemonSettings, String> {
    let saved = match rpc(Command::SetConfig { settings }).await? {
        Reply::Config { settings } => settings,
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

fn main() {
    tauri::Builder::default()
        .manage(GuiPrefs::default())
        .invoke_handler(tauri::generate_handler![
            list_tasks,
            add_task,
            add_video_task,
            pause_task,
            resume_task,
            update_selection,
            remove_task,
            pick_dir,
            pick_file,
            torrent_details,
            open_path,
            download_root_path,
            get_config,
            set_config
        ])
        .setup(|app| {
            use tauri::Manager;

            // Sync the tray preference from the daemon settings.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(Reply::Config { settings }) = rpc(Command::GetConfig).await {
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
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
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
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
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
                    .is_some_and(|p| p.hide_to_tray.load(std::sync::atomic::Ordering::Relaxed));
                if hide && window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running ripdl gui");
}
