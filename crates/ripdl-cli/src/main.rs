use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{Command, Reply, Request, Response, DEFAULT_PORT};
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
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
        Cmd::Add { url, dest, header, referer, cookie, user_agent } => {
            let headers = collect_headers(&header, referer, cookie, user_agent)?;
            rpc(Command::Add { url, dest_dir: dest, headers }).await
        }
        Cmd::Ls => rpc(Command::List).await,
        Cmd::Pause { id } => rpc(Command::Pause { id }).await,
        Cmd::Resume { id } => rpc(Command::Resume { id }).await,
        Cmd::Rm { id } => rpc(Command::Remove { id }).await,
        Cmd::Stop => rpc(Command::Shutdown).await,
    }
}

/// One-shot RPC client: connect, send, print the reply.
async fn rpc(cmd: Command) -> anyhow::Result<()> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", DEFAULT_PORT))
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
            for t in tasks {
                let progress = match t.total {
                    Some(total) => format!(
                        "{}/{} ({:.0}%)",
                        humans(t.downloaded),
                        humans(total),
                        t.downloaded as f64 / total as f64 * 100.0
                    ),
                    None => humans(t.downloaded),
                };
                let speed = t
                    .speed_bps
                    .map(|s| format!("{}/s", humans(s as u64)))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<38} {:<10} {:>16} {:>12}",
                    t.id,
                    format!("{:?}", t.state).to_lowercase(),
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
}
