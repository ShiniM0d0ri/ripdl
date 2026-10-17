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
    },
    /// Show what the engine plans for a URL (size, ranges, filename).
    Info { url: String },
    /// Queue a URL with the running daemon.
    Add {
        url: String,
        /// Output directory (default: your downloads folder).
        #[arg(short, long)]
        dest: Option<PathBuf>,
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
        Cmd::Download { url, out, segments } => {
            let mut cfg = EngineConfig::default();
            if let Some(n) = segments {
                cfg.max_segments = n;
            }
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
        Cmd::Add { url, dest } => rpc(Command::Add { url, dest_dir: dest }).await,
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
