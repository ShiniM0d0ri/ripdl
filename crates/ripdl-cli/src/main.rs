use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{Command, Reply, Request, Response};
use ripdl_core::{Engine, EngineConfig};

#[derive(Parser)]
#[command(
    name = "ripdl",
    version,
    about = "Free, ad-free, multi-segment download manager"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download a URL now, bypassing the daemon (multi-segment, resumable).
    Download {
        url: String,
        /// Output directory (default: current directory).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Max parallel segments.
        #[arg(short, long)]
        segments: Option<usize>,
        /// Global speed cap for the whole download, e.g. `500k`, `2.5m`,
        /// `1g`, or plain bytes/sec. Minimum cap is 1k.
        #[arg(short, long = "max-speed")]
        max_speed: Option<String>,
        /// Extra request header `Name: value`; repeatable. Needed for
        /// cookies, referers, auth, and hotlink-protected links.
        #[arg(short = 'H', long = "header")]
        header: Vec<String>,
        /// Shorthand for --header "Referer: <url>".
        #[arg(long)]
        referer: Option<String>,
        /// Shorthand for --header "Cookie: <value>".
        #[arg(long)]
        cookie: Option<String>,
        /// Shorthand for --header "User-Agent: <value>".
        #[arg(short = 'u', long = "user-agent")]
        user_agent: Option<String>,
    },
    /// Show what the engine plans for a URL (size, ranges, filename).
    Info { url: String },
    /// Queue a URL with the running daemon.
    Add {
        url: String,
        /// Output directory (default: your downloads folder).
        #[arg(short, long)]
        dest: Option<PathBuf>,
        /// Extra request header `Name: value`; repeatable.
        #[arg(short = 'H', long = "header")]
        header: Vec<String>,
        /// Shorthand for --header "Referer: <url>".
        #[arg(long)]
        referer: Option<String>,
        /// Shorthand for --header "Cookie: <value>".
        #[arg(long)]
        cookie: Option<String>,
        /// Shorthand for --header "User-Agent: <value>".
        #[arg(short = 'u', long = "user-agent")]
        user_agent: Option<String>,
        /// Start the download after a delay, e.g. `30s`, `1h30m`, `2d`.
        #[arg(long = "after")]
        after: Option<String>,
        /// Start at local wall-clock time `HH:MM` (today, or tomorrow if past).
        #[arg(long = "at")]
        at: Option<String>,
        /// Hunt mode: treat the URL as a video/stream page and hand it to
        /// the yt-dlp sidecar. Needs yt-dlp on `PATH` or `ytdlp_path` in
        /// the daemon's config.json.
        #[arg(long = "video")]
        video: bool,
    },
    /// List tasks known to the daemon.
    Ls,
    /// Pause a running task.
    Pause { id: String },
    /// Resume a paused or failed task.
    Resume { id: String },
    /// Remove a task from the queue.
    Rm { id: String },
    /// Stop the daemon.
    Stop,
    /// Register this binary as the browser's native messaging host
    /// (`ripdl host` mode triggers when Chrome passes an extension origin).
    RegisterHost {
        /// Extension id from chrome://extensions -> Developer mode -> ID.
        #[arg(long)]
        ext_id: String,
        /// Native host name (reverse dotted name; org.ripdl.host).
        #[arg(long, default_value = "org.ripdl.host")]
        host: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Chrome native messaging launches us as `ripdl.exe chrome-extension://<id>/`
    // before clap would see one; stdio is the protocol, nothing else may write.
    if std::env::args().nth(1).unwrap_or_default().starts_with("chrome-extension://") {
        match ripdl_core::bridge::serve(ripdl_core::bridge::BridgeConfig::default()) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("native host loop failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Download { url, out, segments, max_speed, header, referer, cookie, user_agent } => {
            let mut cfg = EngineConfig::default();
            if let Some(n) = segments {
                cfg.max_segments = n;
            }
            if let Some(s) = max_speed {
                cfg.max_speed = Some(parse_speed(&s)?);
            }
            cfg.headers = collect_headers(&header, referer, cookie, user_agent)?;
            let engine = Arc::new(Engine::new(cfg)?);

            let info = engine.plan(&url).await.context("failed to probe url")?;

            println!("target:   {url}");
            println!("filename: {}", info.filename);
            if let Some(total) = info.total {
                println!("size:     {}", humans(total));
            }
            println!(
                "ranges:   {}",
                if info.accept_ranges { "yes (accelerated)" } else { "no (single stream)" }
            );

            let bar = match info.total {
                Some(total) => ProgressBar::new(total),
                None => ProgressBar::new_spinner(),
            };
            bar.set_style(
                ProgressStyle::with_template(
                    "  [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec})",
                )?
                .progress_chars("█▉▊▋▌▍▎▏ "),
            );

            let pb = bar.clone();
            let token = CancellationToken::new();
            let dest = engine
                .download(&url, out, token, move |p| pb.set_position(p.downloaded))
                .await
                .context("download failed")?;
            bar.finish_and_clear();
            println!("saved to: {}", dest.display());
            Ok(())
        }
        Cmd::Info { url } => {
            let engine = Engine::new(EngineConfig::default())?;
            let info = engine.plan(&url).await.context("failed to probe url")?;
            println!("{info:#?}");
            Ok(())
        }
        Cmd::Add { url, dest, header, referer, cookie, user_agent, after, at, video } => {
            let start_at = match (&after, &at) {
                (Some(_), Some(_)) => bail!("use --after or --at, not both"),
                (Some(delay), None) => Some(now_unix() + parse_after(delay)? as i64),
                (None, Some(when)) => Some(parse_at(when)?),
                (None, None) => None,
            };
            let headers = collect_headers(&header, referer, cookie, user_agent)?;
            let kind = if video {
                ripdl_core::task::TaskKind::Video
            } else {
                ripdl_core::task::TaskKind::Http
            };
            rpc(Command::Add { url, dest_dir: dest, headers, start_at, kind }).await
        }
        Cmd::RegisterHost { ext_id, host } => register_host(&ext_id, &host),
        Cmd::Ls => rpc(Command::List).await,
        Cmd::Pause { id } => rpc(Command::Pause { id }).await,
        Cmd::Resume { id } => rpc(Command::Resume { id }).await,
        Cmd::Rm { id } => rpc(Command::Remove { id }).await,
        Cmd::Stop => rpc(Command::Shutdown).await,
    }
}

/// One-shot RPC client: connect, send, print the reply.
async fn rpc(cmd: Command) -> anyhow::Result<()> {
    let mut stream = ripdl_core::transport::connect_daemon()
        .await
        .context("daemon not running (start it with: ripdl-daemon)")?;

    let req = Request { id: 1, cmd };
    let mut line = serde_json::to_string(&req).context("encode request")?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let mut lines = BufReader::new(stream).lines();
    let resp_line = lines
        .next_line()
        .await?
        .context("daemon closed the connection without a reply")?;
    let resp: Response = serde_json::from_str(&resp_line).context("bad reply from daemon")?;

    match resp.reply {
        Reply::Ok => println!("ok"),
        Reply::Added { id } => println!("queued: {id}"),
        Reply::Error { message } => bail!("{message}"),
        Reply::Tasks { tasks } => {
            println!("{:<38} {:<10} {:>16} {:>12}", "ID", "STATE", "PROGRESS", "SPEED");
            let now = now_unix();
            for t in tasks {
                let mut state = format!("{:?}", t.state);
                if let Some(start) = t.start_at {
                    if start > now && matches!(t.state, ripdl_core::TaskState::Queued) {
                        state = "timed".to_string();
                    }
                }
                let progress = match t.percent {
                    // Video tasks report a tool-level percent, not bytes.
                    Some(pct) => format!("{pct:.1}%"),
                    None => match t.total {
                        Some(total) => format!(
                            "{}/{} ({:.0}%)",
                            humans(t.downloaded),
                            humans(total),
                            t.downloaded as f64 / total as f64 * 100.0
                        ),
                        None => humans(t.downloaded),
                    },
                };
                let speed = t
                    .speed_bps
                    .map(|s| format!("{}/s", humans(s as u64)))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<38} {:<10} {:>16} {:>12}",
                    t.id,
                    state.to_lowercase(),
                    progress,
                    speed
                );
                if let Some(err) = t.error {
                    println!("  error: {err}");
                }
            }
        }
    }
    Ok(())
}

fn humans(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Unix seconds since the epoch (used for relative scheduling).
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Parse a delay like `90s`, `30m`, `2h`, `1d`, or compounds `1h30m`, `2d12h`.
fn parse_after(s: &str) -> anyhow::Result<u64> {
    let mut total: u64 = 0;
    let mut rest = s;
    let mut parts = 0;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            bail!("bad --after '{s}' (expected e.g. 30s, 1h30m, 2d)");
        }
        let n: u64 = rest[..digits_end]
            .parse()
            .with_context(|| format!("bad --after number in '{s}'"))?;
        rest = &rest[digits_end..];
        let mult = match rest.chars().next() {
            Some('s') => 1,
            Some('m') => 60,
            Some('h') => 60 * 60,
            Some('d') => 24 * 60 * 60,
            _ => bail!("bad --after unit in '{s}' (use s/m/h/d)"),
        };
        rest = &rest[1..];
        total = total
            .checked_add(n.checked_mul(mult).context("bad --after value")?)
            .context("bad --after value")?;
        parts += 1;
    }
    if parts == 0 || total == 0 {
        bail!("--after needs a positive delay, got '{s}'");
    }
    Ok(total)
}

/// Parse `HH:MM` into unix seconds of the next occurrence in local time.
fn parse_at(s: &str) -> anyhow::Result<i64> {
    let (h, m) = s
        .split_once(':')
        .with_context(|| format!("--at needs 'HH:MM', got '{s}'"))?;
    let hour: u8 = h
        .parse()
        .with_context(|| format!("bad hour in '{s}'"))?;
    let minute: u8 = m
        .parse()
        .with_context(|| format!("bad minute in '{s}'"))?;
    if hour > 23 || minute > 59 {
        bail!("--at '{s}' is not a valid clock time");
    }

    use time::OffsetDateTime;
    let now = OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let slot = time::Time::from_hms(hour, minute, 0).expect("validated h/m");
    let today = now.replace_time(slot);
    let target = if today > now { today } else { today + time::Duration::days(1) };
    Ok(target.unix_timestamp())
}

/// Merge `--header` repeats with the shorthand flags into one header map.
/// Shorthands win over an explicit same-name `--header`.
fn collect_headers(
    header: &[String],
    referer: Option<String>,
    cookie: Option<String>,
    user_agent: Option<String>,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    for h in header {
        let (k, v) = parse_header(h)?;
        out.insert(k, v);
    }
    if let Some(r) = referer {
        out.insert("Referer".to_string(), r);
    }
    if let Some(c) = cookie {
        out.insert("Cookie".to_string(), c);
    }
    if let Some(ua) = user_agent {
        out.insert("User-Agent".to_string(), ua);
    }
    Ok(out)
}

/// Parse one `Name: value` header string.
fn parse_header(s: &str) -> anyhow::Result<(String, String)> {
    let (k, v) = s
        .split_once(':')
        .with_context(|| format!("header '{s}' needs the form 'Name: value'"))?;
    let k = k.trim();
    let v = v.trim();
    if k.is_empty() {
        bail!("empty header name in '{s}'");
    }
    if v.is_empty() {
        bail!("empty header value in '{s}'");
    }
    Ok((k.to_string(), v.to_string()))
}

/// Write the native-messaging manifest and point the browser's registry keys
/// at it (HKCU, so no admin window ever appears).
fn register_host(ext_id: &str, host: &str) -> anyhow::Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
    {
        bail!("host name must be lower-case [a-z0-9.], got '{host}'");
    }
    if ext_id.is_empty() {
        bail!("extension id missing (see chrome://extensions -> Developer mode -> ID)");
    }

    let exe = std::env::current_exe().context("need our absolute path for the manifest")?;
    let manifest_path = ripdl_core::bridge::manifest_path(host)?;
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent).context("create manifest directory")?;
    }
    let manifest = serde_json::json!({
        "name": host,
        "description": "ripdl browser bridge",
        "path": exe.display().to_string(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{ext_id}/")],
    });
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
        .context("write manifest json")?;

    #[cfg(windows)]
    {
        // Chrome and Edge keep per-user native-host entries under HKCU; the
        // default value of the key is the path to our manifest.
        for (browser, subkey) in [
            ("Chrome", r"Software\Google\Chrome\NativeMessagingHosts"),
            ("Edge", r"Software\Microsoft\Edge\NativeMessagingHosts"),
        ] {
            let key = format!(r"HKCU\{subkey}\{host}");
            let out = std::process::Command::new("reg")
                .args(["add", &key, "/ve", "/d", &manifest_path.display().to_string(), "/f"])
                .output()
                .context("failed to run reg (registry registration)")?;
            if !out.status.success() {
                bail!(
                    "reg add failed for {browser}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
        }
        println!("registered for Chrome and Edge (HKCU, per-user)");
    }
    #[cfg(not(windows))]
    {
        println!(
            "manifest written: {}\nplace/symlink it inside your browser's NativeMessagingHosts directory (e.g. ~/.config/google-chrome/NativeMessagingHosts/,{0})",
            manifest_path.display()
        );
    }

    println!("host manifest: {}", manifest_path.display());
    println!("next: load the extension unpacked (chrome://extensions -> Developer mode -> Load unpacked), then reload it after every `cargo build`");
    Ok(())
}

/// Parse a human speed cap like `500k`, `2.5m`, `1g`, or plain bytes/sec.
/// Caps below 1 KiB/s are rejected as obviously accidental.
fn parse_speed(s: &str) -> anyhow::Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    let cut = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let mult = match s[cut.len()..].trim() {
        "" => 1u64,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        other => bail!("unknown speed suffix '{other}' (use k, m, or g)"),
    };
    let v: f64 = cut.trim().parse().context("invalid speed number")?;
    if v < 0.0 {
        bail!("speed cannot be negative");
    }
    let bps = (v * mult as f64) as u64;
    if bps < 1024 {
        bail!("minimum speed cap is 1k");
    }
    Ok(bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_speed_caps() {
        assert_eq!(parse_speed("500k").unwrap(), 500 * 1024);
        assert_eq!(parse_speed("2.5m").unwrap(), (2.5 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_speed("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_speed("1048576").unwrap(), 1024 * 1024);
    }

    #[test]
    fn rejects_bad_speed_caps() {
        assert!(parse_speed("900").is_err(), "below 1k floor");
        assert!(parse_speed("500x").is_err(), "unknown suffix");
        assert!(parse_speed("-2m").is_err(), "negative");
        assert!(parse_speed("abc").is_err(), "not a number");
    }

    #[test]
    fn parses_header_name_value() {
        assert_eq!(parse_header("Referer: https://x.y").unwrap(), ("Referer".into(), "https://x.y".into()));
        assert!(parse_header("novalue").is_err());
        assert!(parse_header("Name:").is_err(), "empty value rejected");
        assert!(parse_header(": value").is_err(), "empty name rejected");
    }

    #[test]
    fn parses_after_delays() {
        assert_eq!(parse_after("90s").unwrap(), 90);
        assert_eq!(parse_after("30m").unwrap(), 1800);
        assert_eq!(parse_after("2h").unwrap(), 7200);
        assert_eq!(parse_after("1d").unwrap(), 86400);
        assert_eq!(parse_after("1h30m").unwrap(), 5400);
        assert_eq!(parse_after("2d12h").unwrap(), 216000);
        assert!(parse_after("").is_err());
        assert!(parse_after("0").is_err(), "zero delay rejected");
        assert!(parse_after("5x").is_err(), "unknown unit rejected");
        assert!(parse_after("hm").is_err(), "missing digits rejected");
        assert!(parse_after("30d1").is_err(), "trailing digits rejected");
    }

    #[test]
    fn parses_at_clock_times() {
        // Next occurrence is always strictly in the future, within 25 h.
        let now = now_unix();
        let when = parse_at("03:05").unwrap();
        assert!(when > now, "next occurrence must be in the future");
        assert!(when - now <= 25 * 3600, "at most tomorrow, got {when}");
        assert!(parse_at("24:00").is_err());
        assert!(parse_at("23:60").is_err());
        assert!(parse_at("abc").is_err());
        assert!(parse_at("1230").is_err());
    }
}
