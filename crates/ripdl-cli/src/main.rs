use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{Command, Reply, Request, Response};
use ripdl_core::{Engine, EngineConfig};

/// Lowercase label for one `ls` row (avoids `{state:?}` + `to_lowercase`
/// churn per line - Debug prints come back as "Running", not "running").
fn state_label(s: ripdl_core::TaskState) -> &'static str {
    use ripdl_core::TaskState::*;
    match s {
        Queued => "queued",
        Running => "running",
        Paused => "paused",
        Failed => "failed",
        Completed => "completed",
        Cancelled => "cancelled",
    }
}

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
        /// Spread this download's segments over every internet-connected
        /// interface (Wi-Fi + Ethernet + tethering...) to combine their
        /// bandwidth.
        #[arg(long)]
        bond: bool,
    },
    /// Show what the engine plans for a URL (size, ranges, filename).
    Info { url: String },
    /// Queue a URL with the running daemon.
    Add {
        url: String,
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
        /// Torrent mode: magnets, local .torrent files and .torrent URLs
        /// are auto-detected; this flag forces it otherwise.
        #[arg(long = "torrent")]
        torrent: bool,
        /// Torrent-only: fetch pieces front-to-back (streaming order, so
        /// the file head plays before the whole task completes).
        #[arg(long = "sequential")]
        sequential: bool,
        /// Torrent-only: fetch only these file indices (comma-separated,
        /// based on `ripdl details` order; e.g. `--only 0,2-4`).
        #[arg(long = "only")]
        only: Option<String>,
    },
    /// List tasks known to the daemon.
    Ls,
    /// Pause a running task.
    Pause { id: String },
    /// Resume a paused or failed task.
    Resume { id: String },
    /// Remove a task from the queue.
    Rm {
        id: String,
        /// Also delete the downloaded file(s) from disk (default: keep).
        #[arg(long = "files")]
        files: bool,
    },
    /// Details for one task: torrents show files/peers/trackers, HTTP/video
    /// tasks show the address, output file and byte-range segments.
    Details { id: String },
    /// Point a paused/failed task at a fresh URL (expired signed link keeps
    /// the same file). Bytes already on disk are kept; the new address must
    /// serve the same file (name + size) and support byte ranges.
    Refresh {
        id: String,
        url: String,
        /// Resume the download right after the swap.
        #[arg(long)]
        resume: bool,
    },
    /// Change a torrent task's per-file selection: file indices
    /// comma-separated (`0,2,3`) or `all`.
    Select { id: String, files: String },
    /// Show the daemon's settings (config.json contents).
    Config {
        /// `key=value` pairs to change (e.g. `max_concurrent=4`); omit to
        /// just show the current settings. Valid keys: max_concurrent,
        /// max_segments, min_segment_size, max_speed_bytes (0 = uncapped),
        /// default_download_dir, ytdlp_path, hide_to_tray, bond_interfaces,
        /// update_check, update_auto_install, seed_enabled (true|false),
        /// seed_ratio (0 = unlimited), seed_time_min (0 = never).
        #[arg(value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// Stop the daemon.
    Stop,
    /// Check GitHub for a newer ripdl release and print the release page
    /// URL if one exists. Never downloads anything; needs no daemon.
    Update {
        /// Also download the SHA256-verified installer and run it to
        /// upgrade in place (Windows; goes through the daemon).
        #[arg(long)]
        install: bool,
    },
    /// Register this binary as the browser's native messaging host
    /// (`ripdl host` mode triggers when Chrome passes an extension origin).
    RegisterHost {
        /// Extra extension IDs to allow besides the pinned one (from
        /// chrome://extensions -> Developer mode -> ID, e.g. a dev build).
        /// Repeatable. The pinned ID is always allowed.
        #[arg(long)]
        ext_id: Vec<String>,
        /// Native host name (reverse dotted name; org.ripdl.host).
        #[arg(long, default_value = "org.ripdl.host")]
        host: String,
        /// Register even when this is a dev build (`target\debug` /
        /// `target\release`) that would replace a registration pointing at an
        /// installed ripdl.
        #[arg(long)]
        force: bool,
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
        Cmd::Download { url, out, segments, max_speed, header, referer, cookie, user_agent, bond } => {
            let mut cfg = EngineConfig::default();
            if let Some(n) = segments {
                cfg.max_segments = n;
            }
            if let Some(s) = max_speed {
                cfg.max_speed = Some(parse_speed(&s)?);
            }
            if bond {
                cfg.bond = ripdl_core::Bond::Auto;
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
        Cmd::Add { url, dest, header, referer, cookie, user_agent, after, at, video, torrent, sequential, only } => {
            let start_at = match (&after, &at) {
                (Some(_), Some(_)) => bail!("use --after or --at, not both"),
                (Some(delay), None) => Some(now_unix() + parse_after(delay)? as i64),
                (None, Some(when)) => Some(parse_at(when)?),
                (None, None) => None,
            };
            let headers = collect_headers(&header, referer, cookie, user_agent)?;
            let kind = if video {
                ripdl_core::task::TaskKind::Video
            } else if torrent
                || ripdl_core::torrent::looks_like_torrent_payload(&url)
            {
                ripdl_core::task::TaskKind::Torrent
            } else {
                ripdl_core::task::TaskKind::Http
            };
            let only_files = match &only {
                Some(spec) if kind == ripdl_core::task::TaskKind::Torrent => {
                    Some(parse_only(spec)?)
                }
                Some(_) => bail!("--only applies to torrent downloads only"),
                None => None,
            };
            rpc(Command::Add {
                url,
                dest_dir: dest,
                headers,
                start_at,
                kind,
                sequential,
                only_files,
            })
            .await
        }
        Cmd::RegisterHost { ext_id, host, force } => register_host(&ext_id, &host, force),
        Cmd::Ls => rpc(Command::List).await,
        Cmd::Pause { id } => rpc(Command::Pause { id }).await,
        Cmd::Resume { id } => rpc(Command::Resume { id }).await,
        Cmd::Rm { id, files } => rpc(Command::Remove { id, delete_files: files }).await,
        Cmd::Details { id } => rpc(Command::Details { id }).await,
        Cmd::Refresh { id, url, resume } => {
            let reply = rpc_reply(Command::RefreshUrl { id: id.clone(), url, resume }).await?;
            match reply {
                Reply::Refreshed { kept, total, .. } => {
                    println!(
                        "address refreshed: keeping {} of {} already on disk",
                        humans(kept),
                        total.map(humans).unwrap_or_else(|| "?".to_string())
                    );
                    if !resume {
                        println!("resume it with: ripdl resume {id}");
                    }
                    Ok(())
                }
                other => print_reply(other),
            }
        }
        Cmd::Select { id, files } => {
            let only_files = if files.trim().eq_ignore_ascii_case("all") {
                None
            } else {
                let mut v = Vec::new();
                for part in files.split(',') {
                    let part = part.trim();
                    let idx: usize =
                        part.parse().with_context(|| format!("bad file index {part:?}"))?;
                    v.push(idx);
                }
                Some(v)
            };
            rpc(Command::UpdateTorrentSelection { id, only_files }).await
        }
        Cmd::Config { set } => {
            if set.is_empty() {
                return rpc(Command::GetConfig).await;
            }
            // Partial edits: fetch the live values and patch on top, then
            // send the whole settings object back (full-replace semantics).
            let mut settings = match rpc_reply(Command::GetConfig).await? {
                Reply::Config { settings, .. } => settings,
                other => bail!("unexpected daemon reply: {other:?}"),
            };
            for pair in set {
                let (key, raw) = pair
                    .trim()
                    .split_once('=')
                    .with_context(|| format!("expected key=value, got {pair:?}"))?;
                let val = raw.trim();
                match key.trim() {
                    "max_concurrent" => {
                        settings.max_concurrent = Some(val.parse().context("max_concurrent needs a number (1..)")?)
                    }
                    "max_segments" => {
                        settings.max_segments = Some(val.parse().context("max_segments needs a number (1..)")?)
                    }
                    "min_segment_size" => {
                        settings.min_segment_size = Some(val.parse().context("min_segment_size needs a number (1..)")?)
                    }
                    "max_speed_bytes" | "max_speed" => {
                        let n = if val == "0" || val.eq_ignore_ascii_case("unlimited") {
                            0
                        } else {
                            parse_speed(val).context("max_speed: 0, or e.g. 2MiB")?
                        };
                        settings.max_speed_bytes = Some(n);
                    }
                    "default_download_dir" => settings.default_download_dir = Some(PathBuf::from(val)),
                    "ytdlp_path" => settings.ytdlp_path = Some(val.to_string()),
                    "hide_to_tray" => {
                        settings.hide_to_tray = Some(val.parse().context("hide_to_tray needs true|false")?)
                    }
                    "bond_interfaces" => {
                        settings.bond_interfaces =
                            Some(val.parse().context("bond_interfaces needs true|false")?)
                    }
                    "update_check" => {
                        settings.update_check =
                            Some(val.parse().context("update_check needs true|false")?)
                    }
                    "update_auto_install" => {
                        settings.update_auto_install =
                            Some(val.parse().context("update_auto_install needs true|false")?)
                    }
                    "seed_enabled" => {
                        settings.seed_enabled =
                            Some(val.parse().context("seed_enabled needs true|false")?)
                    }
                    "seed_ratio" => {
                        settings.seed_ratio =
                            Some(val.parse().context("seed_ratio needs a number (0 = unlimited)")?)
                    }
                    "seed_time_min" => {
                        settings.seed_time_min = Some(
                            val.parse()
                                .context("seed_time_min needs minutes (0 = never)")?,
                        )
                    }
                    other => bail!("unknown setting {other:?} (see: ripdl config --help)"),
                }
            }
            rpc(Command::SetConfig { settings }).await
        }
        Cmd::Stop => rpc(Command::Shutdown).await,
        Cmd::Update { install } => {
            if install {
                // Via the daemon: it owns the version stamp, does the
                // verified download and launches the Inno setup.
                let reply = rpc_reply(Command::UpdateInstall).await?;
                match reply {
                    Reply::Ok => {
                        println!("new setup downloaded (sha256-verified) and launched;");
                        println!("follow the install prompts, then start ripdl again");
                        return Ok(());
                    }
                    Reply::Error { message } => bail!("{message}"),
                    other => bail!("unexpected daemon reply: {other:?}"),
                }
            }
            let info =
                ripdl_core::update::fetch_latest().await.context("update check failed")?;
            if info.update_available {
                println!("installed: {}  latest: {}", info.current, info.latest);
                println!("new release: {}", info.url);
                if let Some(published_at) = info.published_at.as_deref() {
                    println!("published: {published_at}");
                }
                println!("get the Windows setup (`ripdl-v{}-x86_64-windows-setup.exe`) and run it over the old install", info.latest);
            } else {
                println!(
                    "ripdl {} is up to date (latest release: {})",
                    info.current, info.latest
                );
            }
            Ok(())
        }
    }
}

/// One-shot RPC client: connect, send, print the reply.
async fn rpc(cmd: Command) -> anyhow::Result<()> {
    print_reply(rpc_reply(cmd).await?)
}

/// Transport only: connect, send, decode the reply without printing.
async fn rpc_reply(cmd: Command) -> anyhow::Result<Reply> {
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
        Reply::Error { message } => bail!("{message}"),
        other => Ok(other),
    }
}

fn print_reply(reply: Reply) -> anyhow::Result<()> {
    match reply {
        Reply::Ok => println!("ok"),
        Reply::Added { id } => println!("queued: {id}"),
        Reply::Error { message } => bail!("{message}"),
        Reply::Config { settings, torrent_session, torrent_bind } => {
            println!("max_concurrent:       {}", settings.max_concurrent.unwrap_or(3));
            println!("max_segments:         {}", settings.max_segments.unwrap_or(8));
            println!(
                "min_segment_size:     {}",
                humans(settings.min_segment_size.unwrap_or(1024 * 1024))
            );
            println!(
                "max_speed:            {}",
                settings
                    .max_speed_bytes
                    .filter(|&b| b > 0)
                    .map(humans)
                    .unwrap_or_else(|| "unlimited".to_string())
            );
            println!(
                "default_download_dir: {}",
                settings
                    .default_download_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(user Downloads)".to_string())
            );
            println!(
                "ytdlp_path:           {}",
                settings.ytdlp_path.as_deref().unwrap_or("yt-dlp")
            );
            println!("hide_to_tray:         {}", settings.hide_to_tray.unwrap_or(true));
            println!("bond_interfaces:      {}", settings.bond_interfaces.unwrap_or(false));
            // What bonding would actually spread connections across (same
            // discovery the daemon uses). One uplink = a no-op; a second
            // dead-end adapter is the classic "combining got slower" cause.
            let nics = ripdl_core::engine::bonded_nics();
            let list = if nics.is_empty() {
                "(none detected)".to_string()
            } else {
                nics.iter()
                    .map(|n| {
                        format!(
                            "{} ({}{})",
                            n.name,
                            n.addr,
                            if n.is_default_route { ", default route" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" \u{b7} ")
            };
            println!("uplinks:              {list}");
            if settings.bond_interfaces() && nics.len() < 2 {
                println!("note:                 only one uplink - combining cannot add bandwidth");
            }
            // Torrents share one session whose binding is fixed at creation;
            // show what is actually live, not just the config value.
            let bind: Vec<std::net::IpAddr> = torrent_bind
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect();
            println!(
                "torrent bonding:      {}",
                ripdl_core::engine::describe_torrent_bond(
                    settings.bond_interfaces(),
                    torrent_session,
                    &bind
                )
            );
            println!("update_check:         {}", settings.update_check.unwrap_or(true));
            println!("update_auto_install:  {}", settings.update_auto_install.unwrap_or(false));
            println!("seed_enabled:         {}", settings.seed_enabled());
            println!("seed_ratio:           {}", settings.seed_ratio());
            println!("seed_time_min:        {}", settings.seed_time_min());
        }
        Reply::TorrentDetails { details } => {
            println!("name: {}", details.name.as_deref().unwrap_or("(unknown)"));
            println!("hash: {}", details.info_hash);
            if let Some(root) = &details.root {
                println!("root: {}", root.display());
            }
            let phase = if details.finished {
                if details.seeding { "seeding" } else { "finished" }
            } else {
                "running"
            };
            println!(
                "state: {} ({} B / {} B){}",
                phase,
                details.downloaded,
                details.total,
                if details.paused { " [paused]" } else { "" }
            );
            let ratio = if details.downloaded > 0 {
                details.uploaded as f64 / details.downloaded as f64
            } else {
                0.0
            };
            println!(
                "uploaded: {} (ratio {:.2}) / {} peers / up {}",
                humans(details.uploaded),
                ratio,
                details.peers_connected,
                humans(details.upload_speed_bps as u64)
            );
            if !details.trackers.is_empty() {
                println!("trackers:");
                for t in &details.trackers { println!("  {t}"); }
            }
            if !details.files.is_empty() {
                println!("files:");
                for f in &details.files {
                    println!("  {:>9} {:>3}  {}", humans(f.size), if f.included { "yes" } else { "no " }, f.name);
                }
            }
            println!("peers: {} connected", details.peers.len());
            for p in details.peers.iter().take(12) {
                println!("  {:>21} {:>12} {:<28} dl {:>12} ul {:>12}", p.addr, p.state, p.client.as_deref().unwrap_or("-"), p.downloaded, p.uploaded);
            }
        }
        Reply::TaskDetails { details } => {
            let kind = match details.kind {
                ripdl_core::task::TaskKind::Video => "video (yt-dlp sidecar)",
                ripdl_core::task::TaskKind::Torrent => "torrent",
                ripdl_core::task::TaskKind::Http => "http (multi-segment)",
            };
            println!("kind: {kind}");
            println!("state: {}", state_label(details.state));
            println!("url: {}", details.url);
            if let Some(f) = &details.dest_file {
                println!("file: {}", f.display());
            }
            let pct = details
                .percent
                .or_else(|| details.total.map(|t| details.downloaded as f64 / t as f64 * 100.0));
            println!(
                "progress: {}{}",
                match details.total {
                    Some(total) => format!("{} / {}", humans(details.downloaded), humans(total)),
                    None => humans(details.downloaded),
                },
                pct.map(|p| format!(" ({p:.1}%)")).unwrap_or_default()
            );
            if let Some(etag) = &details.etag {
                println!("etag: {etag}");
            }
            if let Some(err) = &details.error {
                println!("error: {err}");
            }
            if !details.segments.is_empty() {
                println!("segments: {}", details.segments.len());
                for s in &details.segments {
                    let span = s.end.saturating_sub(s.start).saturating_add(1);
                    let pct = if s.end == u64::MAX {
                        0.0
                    } else {
                        s.done as f64 / span as f64 * 100.0
                    };
                    println!(
                        "  #{:<2} from {:>12}  {:>12} / {:>12} ({:>5.1}%)",
                        s.index,
                        humans(s.start),
                        humans(s.done),
                        if s.end == u64::MAX {
                            "to EOF".to_string()
                        } else {
                            humans(span)
                        },
                        pct
                    );
                }
            }
        }
        Reply::Refreshed { kept, total, .. } => {
            println!(
                "address refreshed: keeping {} of {} already on disk",
                humans(kept),
                total.map(humans).unwrap_or_else(|| "?".to_string())
            );
        }
        Reply::TorrentPreview { preview } => {
            println!("name: {}", preview.name.as_deref().unwrap_or("(unknown)"));
            println!("hash: {}", preview.info_hash);
            for (i, f) in preview.files.iter().enumerate() {
                println!("  {:>3} {:>10}  {}", i, humans(f.size), f.name);
            }
            println!(
                "total: {}",
                if preview.files.len() > 1 {
                    format!("{} files, {} bytes", preview.files.len(), preview.total)
                } else {
                    format!("{}", preview.total.max(preview.files.first().map(|f| f.size).unwrap_or(0)))
                }
            );
        }
        Reply::Tasks { tasks } => {
            println!("{:<38} {:<10} {:>16} {:>12}", "ID", "STATE", "PROGRESS", "SPEED");
            let now = now_unix();
            for t in tasks {
                let mut state = state_label(t.state).to_string();
                if t.state == ripdl_core::TaskState::Completed && t.seeding {
                    state = "seeding".to_string();
                }
                if t.state == ripdl_core::TaskState::Running && t.verifying {
                    // Hash check: bytes move at disk speed, not download speed.
                    state = "verifying".to_string();
                }
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
                let speed = if t.state == ripdl_core::TaskState::Completed && t.seeding {
                    t.upload_bps
                        .map(|s| format!("\u{2191} {}/s", humans(s as u64)))
                        .unwrap_or_else(|| "\u{2191} -".to_string())
                } else {
                    t.speed_bps
                        .map(|s| format!("{}/s", humans(s as u64)))
                        .unwrap_or_else(|| "-".to_string())
                };
                println!(
                    "{:<38} {:<10} {:>16} {:>12}",
                    t.id,
                    state,
                    progress,
                    speed
                );
                if let Some(err) = t.error {
                    println!("  error: {err}");
                }
            }
        }
        Reply::Update { info } => {
            // Normally reached only via `ripdl update`, which bypasses the
            // daemon; the daemon path can still answer a stray CheckUpdate.
            if info.update_available {
                println!("update available: {} -> {}", info.current, info.latest);
                println!("get it: {}", info.url);
            } else {
                println!("ripdl {} is up to date", info.current);
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
fn register_host(ext_ids: &[String], host: &str, force: bool) -> anyhow::Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
    {
        bail!("host name must be lower-case [a-z0-9.], got '{host}'");
    }

    // allowed_origins: the pinned ID, whatever the caller adds, and whatever
    // previous registrations allowed — re-registering must never strand an
    // extension that worked before.
    let mut origins = vec![format!("chrome-extension://{}/", ripdl_core::bridge::PINNED_EXT_ID)];
    for id in ext_ids {
        validate_ext_id(id, false)?;
        let o = format!("chrome-extension://{id}/");
        if !origins.contains(&o) {
            origins.push(o);
        }
    }

    let exe = std::env::current_exe().context("need our absolute path for the manifest")?;
    let manifest_path = ripdl_core::bridge::manifest_path(host)?;
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent).context("create manifest directory")?;
    }
    let existing = std::fs::read(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if let Some(existing) = &existing {
        // Re-registrations must never strand an extension that worked
        // before: carry the previous origins forward (shape-checked).
        if let Some(list) = existing["allowed_origins"].as_array() {
            for o in list.iter().filter_map(|v| v.as_str()) {
                let Some(rest) = o.strip_prefix("chrome-extension://") else {
                    continue;
                };
                if !rest.ends_with('/') {
                    continue;
                }
                let id = rest.trim_end_matches('/');
                if validate_ext_id(id, true).is_ok() && !origins.contains(&o.to_string()) {
                    origins.push(o.to_string());
                }
            }
        }
    }

    // A dev build must not silently take over the browser integration of an
    // installed ripdl (that is how a stale `target\debug\ripdl.exe` ended up
    // serving a release install). Explicit `--force` keeps E2E testing alive.
    if !force {
        if let Some(previous) = existing
            .as_ref()
            .and_then(|m| m["path"].as_str())
            .map(std::path::PathBuf::from)
        {
            if dev_build_path(&exe)
                && previous != exe
                && previous.exists()
                && !dev_build_path(&previous)
            {
                bail!(
                    "refusing to point the browser host at a dev build\n  new:      {}\n  installed: {}\n  re-run with --force if this is intentional",
                    exe.display(),
                    previous.display()
                );
            }
        }
    }
    if dev_build_path(&exe) {
        eprintln!(
            "warning: registering a DEV build ({}) - browser downloads will use it until the installed ripdl re-registers",
            exe.display()
        );
    }

    let manifest = serde_json::json!({
        "name": host,
        "description": "ripdl browser bridge",
        "path": exe.display().to_string(),
        "type": "stdio",
        "allowed_origins": origins,
    });
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
        .context("write manifest json")?;

    #[cfg(windows)]
    {
        // Per-user native-host entries under HKCU; the default value of the
        // key is the path to our manifest. Everything Chromium-based we care
        // about reads its own subtree (Brave keeps one under BraveSoftware,
        // unlike Chrome/Edge which share Google's).
        for (browser, subkey) in [
            ("Chrome", r"Software\Google\Chrome\NativeMessagingHosts"),
            ("Edge", r"Software\Microsoft\Edge\NativeMessagingHosts"),
            ("Brave", r"Software\BraveSoftware\Brave\NativeMessagingHosts"),
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
        println!("registered for Chrome, Edge and Brave (HKCU, per-user)");
    }
    #[cfg(not(windows))]
    {
        println!(
            "manifest written: {}\nplace/symlink it inside your browser's NativeMessagingHosts directory (e.g. ~/.config/google-chrome/NativeMessagingHosts/,{0})",
            manifest_path.display()
        );
    }

    if origins.len() > 1 {
        println!("allowed extension ids:");
        for o in &origins {
            println!("  {o}");
        }
    }
    println!("host manifest: {}", manifest_path.display());
    if dev_build_path(&exe) {
        println!("next: load the extension unpacked (chrome://extensions -> Developer mode -> Load unpacked), then reload it after every `cargo build`");
        println!("note: an installed ripdl re-registers itself at install time, replacing this dev build");
    } else {
        println!("next: load the extension unpacked (chrome://extensions -> Developer mode -> Load unpacked)");
    }
    Ok(())
}

/// `true` when the exe lives in a cargo build tree (`…\target\debug\x.exe`
/// or `…\target\release\x.exe`): a dev artifact, not an installed copy.
fn dev_build_path(exe: &std::path::Path) -> bool {
    let Some(profile) = exe.parent().and_then(|p| p.file_name()) else {
        return false;
    };
    let profile = profile.to_string_lossy().to_ascii_lowercase();
    if profile != "debug" && profile != "release" {
        return false;
    }
    exe.parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("target"))
        .unwrap_or(false)
}

/// A Chromium extension ID is 32 base-16-ish chars (letters a..p). With an
/// empty input (no --ext-id given) treat it as "nothing extra" instead of
/// an error.
fn validate_ext_id(id: &str, strict: bool) -> anyhow::Result<()> {
    let ok = id.len() == 32 && id.chars().all(|c| ('a'..='p').contains(&c));
    if ok {
        return Ok(());
    }
    if !strict && id.is_empty() {
        return Ok(());
    }
    bail!("extension id must be 32 letters a-p, got '{id}'")
}

/// Parse a human speed cap like `500k`, `2.5m`, `1g`, or plain bytes/sec.
/// Caps below 1 KiB/s are rejected as obviously accidental.
fn parse_speed(s: &str) -> anyhow::Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    let cut = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let mult = match s[cut.len()..].trim() {
        "" => 1u64,
        // Accept the friendly spellings too: kb/kib/k, miB/mib/m, etc.
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
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

fn parse_only(spec: &str) -> anyhow::Result<Vec<usize>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if let Some((a, b)) = part.split_once('-') {
            let a: usize = a.trim().parse().context("bad --only range start")?;
            let b: usize = b.trim().parse().context("bad --only range end")?;
            anyhow::ensure!(a <= b, "--only range start above end");
            out.extend(a..=b);
        } else {
            out.push(part.parse().context("bad --only index")?);
        }
    }
    anyhow::ensure!(!out.is_empty(), "empty --only list");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_build_paths_are_recognized() {
        use std::path::Path;
        // Paths only carry the running platform's separator, so `parent()`
        // only descends on the platform that produced them.
        #[cfg(windows)]
        {
            assert!(dev_build_path(Path::new(r"D:\ripdl\target\debug\ripdl.exe")));
            assert!(dev_build_path(Path::new(r"D:\ripdl\target\release\ripdl.exe")));
            assert!(!dev_build_path(Path::new(
                r"C:\Program Files\ripdl\ripdl.exe"
            )));
            // A "debug"/"release" folder that is not a cargo target dir is a
            // real install location, not a dev artifact.
            assert!(!dev_build_path(Path::new(r"C:\tools\debug\ripdl.exe")));
        }
        #[cfg(not(windows))]
        {
            assert!(dev_build_path(Path::new("/home/x/ripdl/target/release/ripdl")));
            assert!(dev_build_path(Path::new("/home/x/ripdl/target/debug/ripdl")));
            assert!(!dev_build_path(Path::new("/usr/bin/ripdl")));
            assert!(!dev_build_path(Path::new("/opt/ripdl/debug/ripdl")));
        }
        assert!(!dev_build_path(Path::new("ripdl.exe")));
    }

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
    fn parses_only_selections() {
        assert_eq!(parse_only("0").unwrap(), vec![0]);
        assert_eq!(parse_only("0,2").unwrap(), vec![0, 2]);
        assert_eq!(parse_only("2-4").unwrap(), vec![2, 3, 4]);
        assert_eq!(parse_only(" 0 , 2-3 ,5 ").unwrap(), vec![0, 2, 3, 5]);
        assert!(parse_only("3-2").is_err());
        assert!(parse_only("").is_err());
        assert!(parse_only("x").is_err());
    }

    #[test]
    fn parses_header_name_value() {
        assert_eq!(parse_header("Referer: https://x.y").unwrap(), ("Referer".into(), "https://x.y".into()));        assert!(parse_header("novalue").is_err());
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
