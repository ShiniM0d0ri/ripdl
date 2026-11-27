//! Desktop-to-browser integration helpers: managed extension folder sync,
//! Chromium extensions page launch, native-host registration, and plugin install.

use std::path::PathBuf;

use ripdl_core::bridge::PINNED_EXT_ID;
use ripdl_core::extplug;
use ripdl_core::transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserChoice {
    Auto,
    Chrome,
    Edge,
    Brave,
}

impl BrowserChoice {
    pub fn all() -> &'static [Self] {
        &[Self::Auto, Self::Chrome, Self::Edge, Self::Brave]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Default browser",
            Self::Chrome => "Chrome",
            Self::Edge => "Edge",
            Self::Brave => "Brave",
        }
    }

    pub fn display_label(self) -> String {
        match self {
            Self::Auto => match detected_browser_name() {
                Some(name) => format!("Auto ({name})"),
                None => "Default browser".to_string(),
            },
            other => other.label().to_string(),
        }
    }
}

#[derive(Clone, Copy)]
struct ChromiumBrowser {
    name: &'static str,
    extensions_url: &'static str,
}

pub fn managed_extension_dir() -> Result<PathBuf, String> {
    Ok(transport::app_data_dir()
        .map_err(|e| e.to_string())?
        .join("extension"))
}

pub fn bundled_extension_dir() -> Result<PathBuf, String> {
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

pub fn sync_managed_extension() -> Result<extplug::ManagedCopy, String> {
    extplug::sync_managed(&bundled_extension_dir()?, &managed_extension_dir()?)
}

pub fn active_extension_dir(override_dir: &str) -> Result<PathBuf, String> {
    let trimmed = override_dir.trim();
    if trimmed.is_empty() {
        managed_extension_dir()
    } else {
        Ok(PathBuf::from(trimmed))
    }
}

pub fn folder_version(override_dir: &str) -> Option<String> {
    let dir = active_extension_dir(override_dir).ok()?;
    extplug::folder_version(&dir)
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
        Some(ChromiumBrowser {
            name: "Chrome",
            extensions_url: "chrome://extensions",
        })
    } else if prog_id.starts_with("msedgehtm") {
        Some(ChromiumBrowser {
            name: "Edge",
            extensions_url: "edge://extensions",
        })
    } else if prog_id.starts_with("bravehtml") {
        Some(ChromiumBrowser {
            name: "Brave",
            extensions_url: "brave://extensions",
        })
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
        Some(ChromiumBrowser {
            name: "Edge",
            extensions_url: "edge://extensions",
        })
    } else if desktop_id.contains("brave") {
        Some(ChromiumBrowser {
            name: "Brave",
            extensions_url: "brave://extensions",
        })
    } else if desktop_id.contains("chrome") || desktop_id.contains("chromium") {
        Some(ChromiumBrowser {
            name: "Chrome",
            extensions_url: "chrome://extensions",
        })
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn default_chromium_browser() -> Option<ChromiumBrowser> {
    None
}

pub fn detected_browser_name() -> Option<&'static str> {
    default_chromium_browser().map(|b| b.name)
}

fn resolve_browser(choice: BrowserChoice) -> Result<ChromiumBrowser, String> {
    match choice {
        BrowserChoice::Auto => default_chromium_browser()
            .ok_or_else(|| "Select Chrome, Edge, or Brave explicitly (default browser not detected)".into()),
        BrowserChoice::Chrome => Ok(ChromiumBrowser {
            name: "Chrome",
            extensions_url: "chrome://extensions",
        }),
        BrowserChoice::Edge => Ok(ChromiumBrowser {
            name: "Edge",
            extensions_url: "edge://extensions",
        }),
        BrowserChoice::Brave => Ok(ChromiumBrowser {
            name: "Brave",
            extensions_url: "brave://extensions",
        }),
    }
}

#[cfg(windows)]
fn launch_browser_url(browser: ChromiumBrowser, target_url: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let relative = match browser.name {
        "Chrome" => "Google/Chrome/Application/chrome.exe",
        "Edge" => "Microsoft/Edge/Application/msedge.exe",
        "Brave" => "BraveSoftware/Brave-Browser/Application/brave.exe",
        _ => return Err(std::io::Error::other("unsupported browser")),
    };
    let executable_name = relative.rsplit('/').next().unwrap_or_default();
    let mut candidates = Vec::new();
    for hive in ["HKCU", "HKLM"] {
        let key = format!(
            r"{hive}\Software\Microsoft\Windows\CurrentVersion\App Paths\{executable_name}"
        );
        if let Ok(output) = std::process::Command::new("reg")
            .args(["query", &key, "/ve"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(value) = text.lines().find_map(|line| {
                    line.split_once("REG_SZ")
                        .map(|(_, value)| value.trim().trim_matches('"'))
                }) {
                    candidates.push(PathBuf::from(value));
                }
            }
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(root).join(relative));
        }
    }
    let executable = candidates.into_iter().find(|path| path.is_file()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{} executable not found; open {} manually",
                browser.name, target_url
            ),
        )
    })?;
    std::process::Command::new(executable)
        .arg(target_url)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn launch_browser_url(browser: ChromiumBrowser, target_url: &str) -> std::io::Result<()> {
    let programs: &[&str] = match browser.name {
        "Chrome" => &["google-chrome", "chromium", "chromium-browser"],
        "Edge" => &["microsoft-edge", "microsoft-edge-stable"],
        "Brave" => &["brave-browser", "brave"],
        _ => return Err(std::io::Error::other("unsupported browser")),
    };
    for program in programs {
        match std::process::Command::new(program).arg(target_url).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "{} executable not found; open {} manually",
            browser.name, target_url
        ),
    ))
}

pub fn open_extensions_page(override_dir: &str, choice: BrowserChoice) -> Result<String, String> {
    let _ = sync_managed_extension();
    let browser = resolve_browser(choice)?;
    let folder = active_extension_dir(override_dir)?;
    let version = extplug::folder_version(&folder)
        .ok_or_else(|| format!("{} has no readable extension manifest", folder.display()))?;
    launch_browser_url(browser, browser.extensions_url)
        .map_err(|e| format!("could not open {} extensions: {e}", browser.name))?;
    Ok(format!(
        "Opened {} ({}) — select \"{}\" (v{}).",
        browser.name,
        browser.extensions_url,
        folder.display(),
        version
    ))
}

pub fn open_welcome_page(choice: BrowserChoice) -> Result<String, String> {
    let browser = resolve_browser(choice)?;
    let url = format!("chrome-extension://{}/welcome.html", PINNED_EXT_ID);
    launch_browser_url(browser, &url)
        .map_err(|e| format!("could not open welcome page in {}: {e}", browser.name))?;
    Ok(format!("Opened ripdl extension welcome page in {}.", browser.name))
}

pub fn register_host_cli() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("GUI executable has no parent directory")?;
    let bin_name = if cfg!(windows) { "ripdl.exe" } else { "ripdl" };
    let cli = dir.join(bin_name);
    let program = if cli.is_file() {
        cli
    } else {
        PathBuf::from(bin_name)
    };
    let mut cmd = std::process::Command::new(&program);
    cmd.arg("register-host");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().map_err(|e| format!("could not run {}: {e}", program.display()))?;
    if output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if out.is_empty() {
            "Browser native-messaging host registered cleanly.".into()
        } else {
            out.lines().next().unwrap_or("Native host registered.").to_string()
        })
    } else {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(if !err.is_empty() {
            err
        } else if !out.is_empty() {
            out
        } else {
            format!("{} exited with {}", program.display(), output.status)
        })
    }
}

pub fn install_plugin_file(path: &PathBuf, override_dir: &str) -> Result<String, String> {
    let managed = managed_extension_dir()?;
    let target_dir = active_extension_dir(override_dir)?;
    if target_dir == managed {
        let _ = sync_managed_extension();
    }
    let content = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    extplug::install(&target_dir, &name, &content)
}
