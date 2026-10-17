use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use tokio_util::sync::CancellationToken;

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
    /// Download a URL now (multi-segment, resumable).
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

            let info = engine
                .plan(&url)
                .await
                .context("failed to probe url")?;

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
    }
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
