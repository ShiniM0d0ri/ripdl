//! Failure-injection integration tests for the download engine.
//!
//! The mock server is a hand-rolled HTTP/1.1 responder over tokio's
//! `TcpListener`: ranged HEAD/GET support, a deterministic pseudo-random
//! payload, a drop-after-N-bytes knob, an etag the tests can change between
//! attempts, and optional per-block pacing (used to keep reporter flush
//! timing below deterministic, see the cancel-resume test).
//!
//! Everything here is offline; no test touches a real network.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ripdl_core::engine::retry::RetryConfig;
use ripdl_core::{Engine, EngineConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// byte pattern
// ---------------------------------------------------------------------------

/// Deterministic pseudo-random byte for file offset `o`. The server writes
/// this pattern and tests verify the downloaded file byte-for-byte against
/// it, so any lost, repeated, or misplaced write shows up as a specific
/// mismatch instead of a generic failure.
fn pattern_byte(o: u64) -> u8 {
    let mut h = o.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5_5A5A_5A5A_5A5A;
    h ^= h >> 32;
    h ^= h >> 16;
    h as u8
}

fn verify(dest: &Path, total: u64) {
    let bytes = std::fs::read(dest).expect("downloaded file readable");
    assert_eq!(bytes.len() as u64, total, "file size wrong");
    for (i, b) in bytes.iter().enumerate() {
        assert_eq!(*b, pattern_byte(i as u64), "byte {} wrong", i);
    }
}

// ---------------------------------------------------------------------------
// mock server
// ---------------------------------------------------------------------------

struct ServerState {
    total: u64,
    /// Serve 206 ranges (true) or act like a no-range server (false).
    ranges: bool,
    /// Body bytes served on a "dropping" connection before the connection is
    /// killed mid-stream.
    drop_after_bytes: u64,
    /// How many ranged GET connections drop. Only body-carrying ranged GETs
    /// consume the budget, so the HEAD probe never triggers a drop.
    drop_times: usize,
    dropped: AtomicUsize,
    /// Bump between attempts to simulate the file changing on the server.
    etag_version: AtomicUsize,
    /// Sleep inserted between body blocks (pacing, tests 4-5 only).
    block_pace: Option<Duration>,
    /// Every ranged GET's "a-b" request, in connection order.
    ranges_seen: Mutex<Vec<String>>,
}

async fn spawn_server(state: Arc<ServerState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((sock, _)) => {
                    let st = Arc::clone(&state);
                    tokio::spawn(async move {
                        // Errors here are expected: forced drops abort mid-body.
                        let _ = serve(st, sock).await;
                    });
                }
                Err(_) => break,
            }
        }
    });
    format!("http://{addr}/file.bin")
}

fn etag_of(state: &ServerState) -> String {
    format!("\"v{}\"", state.etag_version.load(Ordering::SeqCst))
}

async fn serve(state: Arc<ServerState>, mut sock: TcpStream) -> std::io::Result<()> {
    let mut head: Vec<u8> = Vec::with_capacity(1024);
    let head_len = loop {
        let mut buf = [0u8; 1024];
        let n = sock.read(&mut buf).await?;
        if n == 0 {
            return Ok(()); // client vanished before finishing the request
        }
        head.extend_from_slice(&buf[..n]);
        if let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        if head.len() > 64 * 1024 {
            return Ok(());
        }
    };
    head.truncate(head_len);
    let text = String::from_utf8_lossy(&head);

    let is_head = text.lines().next().unwrap_or("").starts_with("HEAD");
    let mut range: Option<(u64, Option<u64>)> = None;
    let mut if_match = String::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("range:") {
            if let Some(spec) = v.trim().strip_prefix("bytes=") {
                let mut parts = spec.splitn(2, '-');
                let a = parts.next().and_then(|s| s.trim().parse::<u64>().ok());
                let b = parts.next().and_then(|s| s.trim().parse::<u64>().ok());
                if let Some(a) = a {
                    range = Some((a, b));
                }
            }
        } else if let Some(v) = lower.strip_prefix("if-match:") {
            if_match = v.trim().to_string();
        }
    }

    let etag = etag_of(&state);

    // HEAD = engine probe: advertise size + range support, send no body.
    if is_head {
        let ranges_hdr = if state.ranges { "accept-ranges: bytes\r\n" } else { "" };
        let resp = format!(
            "HTTP/1.1 200 OK\r\netag: {etag}\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\n{ranges_hdr}connection: close\r\n\r\n",
            state.total
        );
        sock.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    // Stale resume attempt: the file changed on the server.
    if !if_match.is_empty() && if_match != etag.to_ascii_lowercase() {
        sock.write_all(
            b"HTTP/1.1 412 Precondition Failed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await?;
        return Ok(());
    }

    match range {
        // Ranged GET -> 206 with the exact requested slice.
        Some((a, b)) if state.ranges => {
            if a >= state.total {
                sock.write_all(
                    format!(
                        "HTTP/1.1 416 Range Not Satisfiable\r\ncontent-range: bytes */{}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        state.total
                    )
                    .as_bytes(),
                )
                .await?;
                return Ok(());
            }
            let b = b.unwrap_or(state.total - 1).min(state.total - 1);
            let len = b - a + 1;
            state.ranges_seen.lock().unwrap().push(format!("{a}-{b}"));
            let resp = format!(
                "HTTP/1.1 206 Partial Content\r\netag: {etag}\r\naccept-ranges: bytes\r\ncontent-range: bytes {a}-{b}/{}\r\ncontent-length: {len}\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n",
                state.total
            );
            sock.write_all(resp.as_bytes()).await?;

            let dropping = state.dropped.fetch_add(1, Ordering::SeqCst) < state.drop_times;
            write_body(&mut sock, &state, a, len, dropping).await?;
            Ok(())
        }
        // Plain GET -> the whole body (no-range fallback path).
        _ => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\netag: {etag}\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                state.total
            );
            sock.write_all(resp.as_bytes()).await?;
            write_body(&mut sock, &state, 0, state.total, false).await?;
            Ok(())
        }
    }
}

/// Write `len` patterned bytes starting at file offset `start`; when
/// `dropping`, kill the connection after `drop_after_bytes` bytes (abrupt
/// close, no clean EOF). Honors per-block pacing when set.
async fn write_body(
    sock: &mut TcpStream,
    state: &ServerState,
    start: u64,
    len: u64,
    dropping: bool,
) -> std::io::Result<()> {
    const BLOCK: usize = 16 * 1024;
    let mut block = vec![0u8; BLOCK];
    let mut off = start;
    let mut sent: u64 = 0;
    let mut remaining = len;

    while remaining > 0 {
        if dropping && sent >= state.drop_after_bytes {
            // Hard kill mid-body: the client sees a truncated response.
            return Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted));
        }
        let mut n = (remaining as usize).min(BLOCK);
        if dropping {
            n = n.min((state.drop_after_bytes - sent) as usize).max(1);
        }
        for (i, b) in block[..n].iter_mut().enumerate() {
            *b = pattern_byte(off + i as u64);
        }
        sock.write_all(&block[..n]).await?;
        sock.flush().await?;
        if let Some(pace) = state.block_pace {
            tokio::time::sleep(pace).await;
        }
        off += n as u64;
        sent += n as u64;
        remaining -= n as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test helpers
// ---------------------------------------------------------------------------

fn test_engine() -> Engine {
    Engine::new(EngineConfig {
        max_segments: 8,
        min_segment_size: 128 * KIB,
        retry: RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(200),
        },
        ..EngineConfig::default()
    })
    .expect("engine")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ripdl-it-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn dir_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("list dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

/// 1 MiB, 8 segments of 128 KiB: every byte must land in the right place.
#[tokio::test]
async fn multi_segment_exact_bytes() {
    let state = Arc::new(ServerState {
        total: MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        ranges_seen: Mutex::new(Vec::new()),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("multi");
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download ok");

    verify(&dest, MIB);
    // 1 MiB / max 8 segments / min 128 KiB => exactly 8 ranged connections.
    let seen = state.ranges_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 8, "expected 8 ranges, saw {seen:?}");
    // No sidecar may survive a successful download.
    let leftovers = dir_names(&dest.parent().unwrap());
    assert!(
        !leftovers.iter().any(|n| n.ends_with(".ripdl.json")),
        "sidecar left behind: {leftovers:?}"
    );
}

/// One segment connection is killed 16 KiB in. The engine must recover via
/// its internal retry (ShortSegment / transport error path) and still serve
/// every byte exactly.
#[tokio::test]
async fn dropped_connection_recovered_by_retry() {
    let state = Arc::new(ServerState {
        total: MIB,
        ranges: true,
        drop_after_bytes: 16 * KIB,
        drop_times: 1,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        ranges_seen: Mutex::new(Vec::new()),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("drop");
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download completes after retry");

    verify(&dest, MIB);
}

/// A server without accept-ranges must take the single-stream UNBOUNDED
/// path and still produce an intact file.
#[tokio::test]
async fn no_ranges_falls_back_to_single_stream() {
    let state = Arc::new(ServerState {
        total: MIB,
        ranges: false,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        ranges_seen: Mutex::new(Vec::new()),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("noranges");
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download ok");

    verify(&dest, MIB);
    assert!(
        state.ranges_seen.lock().unwrap().is_empty(),
        "no-range server must not receive ranged GETs"
    );
}

/// Pause at ~256 KiB of durable progress (deterministic thanks to pacing),
/// then a fresh engine call resumes the same file from the sidecar and
/// finishes it. The server must observe ranged GETs that start mid-file.
#[tokio::test]
async fn pause_resumes_from_sidecar() {
    // 4 MiB / 8 segments = 512 KiB each. At 40 ms per 16 KiB block a
    // connection flushes its first 256 KiB block at ~640 ms and finishes at
    // ~1280 ms; the reporter's second tick (~1000 ms) sits safely between
    // those, so the guard below always sees ~2 MiB of durable progress.
    let state = Arc::new(ServerState {
        total: 4 * MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: Some(Duration::from_millis(40)),
        ranges_seen: Mutex::new(Vec::new()),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("resume");

    let token = CancellationToken::new();
    let killer = token.clone();
    let res = engine
        .download(
            &url,
            Some(tmp.clone()),
            token.clone(),
            move |p| {
                if p.downloaded >= 300_000 {
                    killer.cancel();
                }
            },
        )
        .await;
    assert!(
        matches!(res, Err(ripdl_core::Error::Cancelled)),
        "expected cancel, got {res:?}"
    );

    // Sidecar persisted with durable progress.
    let names = dir_names(&tmp);
    assert!(
        names.iter().any(|n| n.ends_with(".ripdl.json")),
        "sidecar missing after cancel: {names:?}"
    );

    // Segments are 512 KiB (4 MiB / 8): a fresh attempt re-requests each
    // segment from its aligned base; a resumed one starts at base + done
    // (unaligned). Slice away the first attempt's requests.
    let seen_before = state.ranges_seen.lock().unwrap().len();

    // Second call resumes and completes.
    let dest = engine
        .download(&url, Some(tmp.clone()), CancellationToken::new(), move |_p| {})
        .await
        .expect("resume completes");

    verify(&dest, 4 * MIB);
    let seen = state.ranges_seen.lock().unwrap().clone();
    let resumed = &seen[seen_before..];
    assert_eq!(resumed.len(), 8, "all 8 segments must re-request, saw {seen:?}");
    for (k, r) in resumed.iter().enumerate() {
        let (a, _) = r.split_once('-').expect("range format");
        let start: u64 = a.parse().expect("range start");
        assert_ne!(
            start % 524_288,
            0,
            "segment {k} must resume mid-segment, not restart, saw {r}"
        );
    }
    let names = dir_names(&dest.parent().unwrap());
    assert!(
        !names.iter().any(|n| n.ends_with(".ripdl.json")),
        "sidecar must be removed on success: {names:?}"
    );
}

/// The whole download's shared byte budget: 1 MiB at 256 KiB/s must take at
/// least ~3s (bucket burst absorbs the first ~1s), proving one limiter gates
/// all 8 segments together.
#[tokio::test]
async fn speed_limit_paces_download() {
    let state = Arc::new(ServerState {
        total: MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        ranges_seen: Mutex::new(Vec::new()),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = Engine::new(EngineConfig {
        max_segments: 8,
        min_segment_size: 128 * KIB,
        retry: RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(200),
        },
        max_speed: Some(256 * KIB),
        ..EngineConfig::default()
    })
    .expect("engine");

    let tmp = temp_dir("paced");
    let start = std::time::Instant::now();
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download ok");
    let elapsed = start.elapsed();

    verify(&dest, MIB);
    assert!(
        elapsed >= Duration::from_secs(3),
        "download escaped the limiter: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_secs(15),
        "limiter grossly overshoots: {elapsed:?}"
    );
}

/// The file changed server-side between attempts: the sidecar's etag no
/// longer matches, so the second call must plan fresh from byte 0 (with an
/// up-to-date If-Match) and complete.
#[tokio::test]
async fn stale_etag_forces_fresh_restart() {
    let state = Arc::new(ServerState {
        total: 4 * MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: Some(Duration::from_millis(40)),
        ranges_seen: Mutex::new(Vec::new()),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("etag");

    // First attempt: pause after durable progress exists.
    let token = CancellationToken::new();
    let killer = token.clone();
    let res = engine
        .download(
            &url,
            Some(tmp.clone()),
            token.clone(),
            move |p| {
                if p.downloaded >= 300_000 {
                    killer.cancel();
                }
            },
        )
        .await;
    assert!(
        matches!(res, Err(ripdl_core::Error::Cancelled)),
        "expected cancel, got {res:?}"
    );

    // The file changed on the server.
    state.etag_version.store(101, Ordering::SeqCst);
    let seen_before = state.ranges_seen.lock().unwrap().len();

    let dest = engine
        .download(&url, Some(tmp.clone()), CancellationToken::new(), move |_p| {})
        .await
        .expect("download completes after etag change");

    verify(&dest, 4 * MIB);
    // Fresh plan: all 8 segments re-request from their aligned segment base
    // (a wrongly resumed attempt would start unaligned mid-segment, and with
    // the stale etag it would have received a terminal 412 and failed).
    let seen = state.ranges_seen.lock().unwrap().clone();
    let fresh = &seen[seen_before..];
    assert_eq!(fresh.len(), 8, "etag change must re-plan all 8 segments, saw {seen:?}");
    for (k, r) in fresh.iter().enumerate() {
        let (a, _) = r.split_once('-').expect("range format");
        let start: u64 = a.parse().expect("range start");
        assert_eq!(
            start % 524_288,
            0,
            "segment {k} must restart at its aligned base, saw {r}"
        );
    }
    let names = dir_names(&dest.parent().unwrap());
    assert!(
        !names.iter().any(|n| n.ends_with(".ripdl.json")),
        "sidecar must be removed on success: {names:?}"
    );
}
