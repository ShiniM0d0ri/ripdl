//! Hunt mode: video/stream tasks delegated to a sidecar `yt-dlp`.
//!
//! ripdl deliberately does not reimplement site extraction (signature
//! solving, IP protection, per-site quirks) - that is `yt-dlp`'s job and a
//! full-time one. This module wraps it as a quiet sidecar process:
//!
//! - spawns `yt-dlp --newline --no-playlist` writing into the task's
//!   destination folder,
//! - parses its progress lines into the same `Progress` stream HTTP tasks
//!   use, so pause/cancel/ls work uniformly,
//! - captures the merged output file,
//! - passes per-task headers through as `--add-header` (cookies/referer for
//!   pages that need them).
//!
//! Cancellation kills the child process; `resume` just re-invokes it - the
//! tool continues partial `.part` files on its own.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::task::Progress;
use crate::Error;

/// Output name template: video title (byte-truncated for filesystem
/// length limits), extension chosen by the tool after merging.
const OUTPUT_TEMPLATE: &str = "%(title).180B.%(ext)s";

/// One parsed `[download]` progress line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HuntEvent {
    /// Completion percent, 0-100.
    pub percent: f64,
    /// Estimated total size in bytes, when the line carries one (`of ~N x`).
    pub total: Option<u64>,
}

/// Everything the sidecar needs for one video task.
#[derive(Debug, Clone)]
pub struct HuntConfig {
    pub id: String,
    /// Executable path or bare name (resolved via `PATH`).
    pub program: String,
    pub url: String,
    pub dest_dir: PathBuf,
    /// Request headers replayed into yt-dlp (cookies, referer, UA...).
    pub headers: HashMap<String, String>,
}

/// Resulting output file, when the tool reported one.
#[derive(Debug, Clone, Default)]
pub struct HuntOutput {
    pub file: Option<PathBuf>,
}

/// Parse one yt-dlp stdout line into a progress event, tolerating the
/// padding variants: `[download]  42.7%`, `[download]   4.5% of  ~ 5.14MiB
/// at ...`, `[download] 100% of 5.14MiB in 00:00:04`.
pub fn parse_progress(line: &str) -> Option<HuntEvent> {
    let rest = line.strip_prefix("[download]")?.trim_start();
    if rest.starts_with("Destination:") {
        return None;
    }
    let pct_end = rest.find('%')?;
    let percent = rest[..pct_end].trim().parse::<f64>().ok()?;
    let total = rest[pct_end + 1..]
        .split_once("of ")
        .and_then(|(_, tail)| total_from_tail(tail));
    Some(HuntEvent { percent, total })
}

/// `~ 5.14MiB at ...` / `~5.14MiB in ...` -> byte count.
fn total_from_tail(tail: &str) -> Option<u64> {
    let mut parts = tail.split_whitespace();
    while let Some(chunk) = parts.next() {
        let mut c = chunk;
        // "~" may be glued to the number or fly separately.
        if c == "~" {
            c = parts.next()?;
        }
        let c = c.trim_start_matches('~');
        if let Some(n) = parse_size(c) {
            return Some(n);
        }
        // Not a size token yet, keep looking (rare formatting).
    }
    None
}

/// `"5.14MiB"` -> bytes. Accepts both IEC (yt-dlp default) and decimal
/// engineer units; sizes are estimates anyway.
fn parse_size(s: &str) -> Option<u64> {
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let num: f64 = s[..num_end].parse().ok()?;
    let mult = match s[num_end..].to_ascii_lowercase().as_str() {
        "b" | "" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((num * mult) as u64)
}

/// Extract the produced file path from `[download] Destination: PATH` and
/// `[Merger] Merging formats into "PATH"` lines.
pub fn parse_output_path(line: &str) -> Option<PathBuf> {
    const MERGE_MARKER: &str = "Merging formats into \"";
    if let Some(idx) = line.find(MERGE_MARKER) {
        let after = &line[idx + MERGE_MARKER.len()..];
        let path = after.split_once('"')?.0;
        return Some(PathBuf::from(path));
    }
    const DEST_MARKER: &str = "Destination: ";
    if let Some(idx) = line.find(DEST_MARKER) {
        let path = line[idx + DEST_MARKER.len()..].trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// The exact yt-dlp invocation for one hunt task, in argument order.
pub fn build_args(cfg: &HuntConfig) -> Vec<String> {
    let mut args = vec![
        "--newline".to_string(),
        "--no-playlist".to_string(),
        "-o".to_string(),
        cfg.dest_dir.join(OUTPUT_TEMPLATE).to_string_lossy().into_owned(),
        // En-bloc so the tool prefers our language for titles/UA alike.
        "--add-header".to_string(),
        "Accept-Language: en-US,en;q=0.8".to_string(),
    ];
    for (k, v) in &cfg.headers {
        if k.is_empty() || v.is_empty() || k.eq_ignore_ascii_case("accept-language") {
            continue;
        }
        args.push("--add-header".to_string());
        args.push(format!("{k}: {v}"));
    }
    args.push("--".to_string());
    args.push(cfg.url.clone());
    args
}

fn spawn_error(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::Hunt("yt-dlp not found - install it or set `ytdlp_path` in config.json".into())
    } else {
        e.into()
    }
}

/// Spawn the sidecar. `program + args` first; on Windows, batch shims
/// (`.cmd`/`.bat`, common wrappers for real yt-dlp installs) retry through
/// `cmd /d /s /c`, since `CreateProcess` does not run batch files itself.
fn spawn_child(program: &str, args: &[String]) -> Result<tokio::process::Child, std::io::Error> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // Never flash a console for a background sidecar.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Ok(child) = cmd.spawn() {
        return Ok(child);
    }
    #[cfg(windows)]
    {
        if is_batch_program(program) {
            let mut cmd = tokio::process::Command::new("cmd.exe");
            cmd.arg("/D").arg("/S").arg("/C");
            cmd.arg(format!("{} {}", quote_arg(program), args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")));
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
            return cmd.spawn();
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("program not found: {program}"),
    ))
}

#[cfg(windows)]
fn is_batch_program(program: &str) -> bool {
    let ext = std::path::Path::new(program)
        .extension()
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    ext == "cmd" || ext == "bat"
}

#[cfg(windows)]
fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    let plain = !arg.contains([' ', '\t', '"']);
    if plain {
        return arg.to_string();
    }
    let mut q = String::with_capacity(arg.len() + 2);
    q.push('"');
    let mut slashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                slashes += 1;
                q.push('\\');
            }
            '"' => {
                // Backslashes before a quote are doubled with the quote.
                for _ in 0..slashes {
                    q.push('\\');
                }
                q.push('\\');
                q.push('"');
                slashes = 0;
            }
            c => {
                slashes = 0;
                q.push(c);
            }
        }
    }
    for _ in 0..slashes {
        q.push('\\');
    }
    q.push('"');
    q
}

/// Run one hunt task to completion. Pushes `Progress` snapshots (percent,
/// optional estimated total) as the tool reports them.
pub async fn download(
    cfg: HuntConfig,
    token: &CancellationToken,
    mut on_progress: impl FnMut(Progress) + Send + 'static,
) -> crate::Result<HuntOutput> {
    if cfg.url.is_empty() {
        return Err(Error::Hunt("empty url".into()));
    }

    let args = build_args(&cfg);
    let mut child = spawn_child(&cfg.program, &args).map_err(spawn_error)?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // Keep the stderr tail for error reports (the interesting failures:
    // unsupported urls, extraction failures, sign-in prompts).
    let stderr_task = tokio::spawn(async move {
        let mut tail = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            tail.push_str(&l);
            tail.push('\n');
            let cut = tail.len().saturating_sub(4000);
            if cut > 0 {
                tail.drain(..cut);
            }
        }
        tail
    });

    // The reader task owns the progress callback (same shape as the
    // engine's reporter) and returns side observations with it.
    let progress = Progress { id: cfg.id.clone(), ..Progress::default() };
    let reader_task = tokio::spawn(async move {
        let mut progress = progress;
        let mut output: Option<PathBuf> = None;
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let Ok(line) = lines.next_line().await else { break };
            let Some(line) = line else { break };
            if let Some(ev) = parse_progress(&line) {
                progress.percent = Some(ev.percent.clamp(0.0, 100.0));
                progress.total = ev.total;
                on_progress(progress.clone());
            }
            if let Some(p) = parse_output_path(&line) {
                output = Some(p);
            }
        }
        output
    });

    let status = tokio::select! {
        _ = token.cancelled() => {
            let _ = child.kill().await;
            drop(child);
            stderr_task.abort();
            reader_task.abort();
            return Err(Error::Cancelled);
        }
        st = child.wait() => st?,
    };

    let output = reader_task.await.unwrap_or_default();
    if status.success() {
        Ok(HuntOutput { file: output })
    } else {
        // Bounded wait for the stderr drain: the interesting failures
        // (unsupported url, extraction failure) surface in its last lines.
        let tail = tokio::time::timeout(std::time::Duration::from_secs(3), stderr_task)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        let mut msg = format!("yt-dlp failed: {status}");
        let trimmed = tail.trim();
        if !trimmed.is_empty() {
            let lines: Vec<&str> = trimmed.lines().collect();
            let start = lines.len().saturating_sub(3);
            let joined = lines[start..].join(" | ");
            msg.push_str(" - ");
            msg.push_str(&joined);
        }
        Err(Error::Hunt(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str) -> HuntConfig {
        HuntConfig {
            id: "t".into(),
            program: "yt-dlp".into(),
            url: url.into(),
            dest_dir: PathBuf::from("D:\\dl"),
            headers: HashMap::new(),
        }
    }

    #[test]
    fn parses_plain_percent() {
        let ev = parse_progress("[download]  42.7%").expect("event");
        assert!((ev.percent - 42.7).abs() < 1e-9);
        assert_eq!(ev.total, None);
    }

    #[test]
    fn parses_percent_with_estimated_total() {
        let ev =
            parse_progress("[download]   4.5% of  ~ 5.14MiB at    1.23MiB/s ETA 00:02")
                .expect("event");
        assert!((ev.percent - 4.5).abs() < 1e-9);
        assert_eq!(ev.total, Some((5.14f64 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn parses_complete_line_exact_total() {
        let ev = parse_progress("[download] 100% of  512.00KiB in 00:00:04").expect("event");
        assert!((ev.percent - 100.0).abs() < 1e-9);
        assert_eq!(ev.total, Some(512 * 1024));
    }

    #[test]
    fn ignores_other_markers() {
        assert_eq!(parse_progress("[download] Destination: D:\\x\\v.mp4"), None);
        assert_eq!(parse_progress("[Merger] Merging formats into \"v.mp4\""), None);
        assert_eq!(parse_progress("[youtube] fetching info"), None);
    }

    #[test]
    fn parses_destination_and_merger_paths() {
        assert_eq!(
            parse_output_path("[download] Destination: D:\\dl\\video.mp4"),
            Some(PathBuf::from("D:\\dl\\video.mp4"))
        );
        assert_eq!(
            parse_output_path(r#"[Merger] Merging formats into "D:\My Videos\clip.mp4""#),
            Some(PathBuf::from("D:\\My Videos\\clip.mp4"))
        );
        assert_eq!(parse_output_path("[download] 42.7%"), None);
    }

    #[test]
    fn args_cover_template_headers_and_url() {
        let mut c = cfg("https://example.com/v");
        c.headers.insert("Cookie".into(), "sid=1".into());
        c.headers.insert("accept-language".into(), "de".into()); // reserved: dropped
        let args = build_args(&c);
        let s = args.join(" ");
        assert!(s.contains("--newline"), "{s}");
        assert!(s.contains("--no-playlist"), "{s}");
        // The `-o` arg must be dest_dir joined with the template; compare via
        // the same std join so the split between the Windows and POSIX path
        // separators (`Path::push` vs `/`) is not asserted by string literal.
        let o = args.iter().position(|a| a == "-o").expect("-o present");
        assert_eq!(args[o + 1], c.dest_dir.join(OUTPUT_TEMPLATE).to_string_lossy().into_owned());
        assert!(s.contains("Cookie: sid=1"), "{s}");
        // Reserved header must not be duplicated from task headers.
        assert_eq!(s.matches("Accept-Language").count(), 1, "{s}");
        assert!(s.ends_with("-- https://example.com/v"), "{s}");
    }

    #[test]
    fn empty_headers_are_dropped() {
        let mut c = cfg("https://example.com/v");
        c.headers.insert("Referer".into(), String::new());
        let args = build_args(&c);
        assert!(!args.join(" ").contains("Referer"));
    }
}
