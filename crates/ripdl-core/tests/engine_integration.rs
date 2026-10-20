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
    /// Content-Disposition advertised by HEAD (filename override path).
    content_disposition: Option<String>,
    /// Every "name: value" header line seen, lowercased name, in arrival
    /// order (headers pass-through verification).
    headers_seen: Mutex<Vec<String>>,
    /// Every ranged GET's "a-b" request, in connection order.
    ranges_seen: Mutex<Vec<String>>,
    /// Source IP of every ranged GET connection (as `"ip"`), in order —
    /// proves which local interfaces the client actually routed through
    /// (NIC bonding assertion).
    peers_seen: Mutex<Vec<String>>,
    /// First N ranged GETs answer 429 (host quota); proves the client backs
    /// off and keeps the task alive instead of failing a segment for good.
    quota_replies: usize,
    quota_served: AtomicUsize,
    /// First N requests of any kind (HEAD probes included) answer 429. Proves
    /// a fresh task retries the probe instead of failing before it starts.
    hard_quota_replies: usize,
    hard_quota_served: AtomicUsize,
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

    // Capture every header for pass-through assertions.
    for line in text.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            state.headers_seen.lock().unwrap().push(format!(
                "{}: {}",
                k.trim().to_ascii_lowercase(),
                v.trim()
            ));
        }
    }

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

    // Host-level quota: refuse the first N requests outright, HEAD included.
    // The probe must treat that as transient instead of failing the task.
    if state.hard_quota_served.fetch_add(1, Ordering::SeqCst) < state.hard_quota_replies {
        sock.write_all(
            b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 1\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await?;
        return Ok(());
    }

    // HEAD = engine probe: advertise size + range support, send no body.
    if is_head {
        let ranges_hdr = if state.ranges { "accept-ranges: bytes\r\n" } else { "" };
        let cd_hdr = state
            .content_disposition
            .as_deref()
            .map(|cd| format!("content-disposition: {cd}\r\n"))
            .unwrap_or_default();
        let resp = format!(
            "HTTP/1.1 200 OK\r\netag: {etag}\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\n{cd_hdr}{ranges_hdr}connection: close\r\n\r\n",
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
            // Host quota: answer the first N ranged GETs with 429. A client
            // that treats status errors as terminal fails the whole task
            // here; the engine must back off (jittered) and retry.
            if state.quota_served.fetch_add(1, Ordering::SeqCst) < state.quota_replies {
                sock.write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 1\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await?;
                return Ok(());
            }
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
            if let Ok(peer) = sock.peer_addr() {
                state.peers_seen.lock().unwrap().push(peer.ip().to_string());
            }
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
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
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

/// Host quota (429) on the first round of range requests: every segment must
/// back off and retry instead of failing the task. This is what OVH does when
/// eight parallel ranges trip its per-IP limit; treating 429 as terminal used
/// to kill the whole download at ~25%.
#[tokio::test]
async fn rate_limited_ranges_back_off_and_finish() {
    let state = Arc::new(ServerState {
        total: 4 * MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        // 4 MiB / 128 KiB / max 8 => 8 segments, each refused once.
        quota_replies: 8,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("quota");
    let dest = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        engine.download(&url, Some(tmp), CancellationToken::new(), move |_p| {}),
    )
    .await
    .expect("quota retries finish in time")
    .expect("download ok despite 429s");

    verify(&dest, 4 * MIB);
    let seen = state.ranges_seen.lock().unwrap();
    assert_eq!(seen.len(), 8, "all 8 ranges retried once: {seen:?}");
    assert_eq!(
        state.quota_served.load(Ordering::SeqCst),
        8 + seen.len(),
        "every 429 was retried"
    );
}

/// A brand-new task must survive a host that answers 429 to the probe itself
/// (HEAD and the 1-byte ranged fallback). Treating the probe as one-shot made
/// a fresh download fail in under a second while the host was only asking for
/// a pause.
#[tokio::test]
async fn probe_retries_host_quota_and_starts() {
    let state = Arc::new(ServerState {
        total: 2 * MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        // HEAD + fallback GET on attempt 1, HEAD on attempt 2.
        hard_quota_replies: 3,
        hard_quota_served: AtomicUsize::new(0),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = test_engine();
    let tmp = temp_dir("probe-quota");
    let dest = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        engine.download(&url, Some(tmp), CancellationToken::new(), move |_p| {}),
    )
    .await
    .expect("probe retry finishes in time")
    .expect("download ok despite a refused probe");

    verify(&dest, 2 * MIB);
    assert!(
        state.hard_quota_served.load(Ordering::SeqCst) >= 3,
        "the probe really was refused first"
    );
}

/// NIC bonding (explicit addresses): every segment must cycle through the
/// two bound loopback addresses (Windows/Linux treat 127.0.0.0/8 as local)
/// and the file must still land byte-exact. The server records each ranged
/// GET's source IP, which is how we observe the binding took effect.
#[tokio::test]
async fn bonded_segments_cycle_interfaces() {
    let state = Arc::new(ServerState {
        total: 512 * KIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let cfg = EngineConfig {
        max_segments: 4,
        min_segment_size: 32 * KIB,
        bond: ripdl_core::Bond::Addresses(vec![
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::net::IpAddr::from([127, 0, 0, 2]),
        ]),
        retry: RetryConfig::default(),
        ..Default::default()
    };
    let engine = Engine::new(cfg).expect("bonded engine");
    let tmp = temp_dir("bonded");
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("bonded download ok");

    verify(&dest, 512 * KIB);
    let mut peers = state.peers_seen.lock().unwrap().clone();
    peers.sort();
    peers.dedup();
    assert!(
        peers.contains(&"127.0.0.1".to_string()) && peers.contains(&"127.0.0.2".to_string()),
        "expected ranged GETs from both bound addresses, saw {peers:?}"
    );
}

/// Bonding configured but only one (non-default) path: the engine keeps
/// OS routing (a single interface offers nothing to combine) and the
/// download still completes byte-exact.
#[tokio::test]
async fn bonded_single_extra_address_still_binds() {
    let state = Arc::new(ServerState {
        total: 256 * KIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let cfg = EngineConfig {
        max_segments: 4,
        min_segment_size: 32 * KIB,
        bond: ripdl_core::Bond::Addresses(vec![std::net::IpAddr::from([127, 0, 0, 2])]),
        ..Default::default()
    };
    let engine = Engine::new(cfg).expect("single-bound engine");
    let tmp = temp_dir("bonded1");
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download ok");
    verify(&dest, 256 * KIB);
    let peers = state.peers_seen.lock().unwrap().clone();
    assert!(!peers.is_empty());
    assert!(
        peers.iter().all(|p| p == "127.0.0.2"),
        "all ranged GETs must come from the explicitly bound address, saw {peers:?}"
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
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
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
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
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
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
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
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
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

/// Single-stream (no ranges) downloads must obey the limiter too: 1.25 MiB
/// through a 256 KiB/s bucket takes at least 1.25 MiB / 256 KiB/s ≈ 5s — the
/// bucket starts empty, so no burst shortcut applies.
#[tokio::test]
async fn speed_limit_applies_to_single_stream() {
    let state = Arc::new(ServerState {
        total: MIB + 256 * KIB,
        ranges: false,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let engine = Engine::new(EngineConfig {
        max_speed: Some(256 * KIB),
        ..EngineConfig::default()
    })
    .expect("engine");

    let tmp = temp_dir("paced-single");
    let start = std::time::Instant::now();
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download ok");
    let elapsed = start.elapsed();

    verify(&dest, MIB + 256 * KIB);
    // Theoretical floor is exactly 5.0s; allow timer-quantization slack.
    // A limiter escape would finish in ~1s, far below this.
    assert!(
        elapsed >= Duration::from_millis(4700),
        "single stream escaped the limiter: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_secs(20),
        "limiter grossly overshoots: {elapsed:?}"
    );
}

/// Refresh download address (IDM's "refresh download address"): a paused
/// task retargeted at a fresh URL - the expired-signed-link case - must
/// continue from the durable offsets instead of restarting, and finish
/// byte-exact.
#[tokio::test]
async fn refreshed_address_resumes_from_sidecar() {
    // Same pacing/size as the resume test: 4 MiB / 8 segments of 512 KiB.
    let state = Arc::new(ServerState {
        total: 4 * MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: Some(Duration::from_millis(40)),
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
    });
    let base = spawn_server(Arc::clone(&state)).await;
    let old = format!("{base}?token=old");
    let new = format!("{base}?token=new");

    let engine = test_engine();
    let tmp = temp_dir("refresh");

    let token = CancellationToken::new();
    let killer = token.clone();
    let res = engine
        .download(&old, Some(tmp.clone()), token, move |p| {
            if p.downloaded >= 300_000 {
                killer.cancel();
            }
        })
        .await;
    assert!(
        matches!(res, Err(ripdl_core::Error::Cancelled)),
        "expected cancel, got {res:?}"
    );

    // The paused sidecar still names the expired address.
    let dest = tmp.join("file.bin");
    let mut st = ripdl_core::refresh::load(&dest)
        .expect("sidecar readable")
        .expect("sidecar exists after cancel");
    assert_eq!(st.url, old);

    // Validate the new address and re-point the sidecar (what the daemon does).
    let info = engine.plan(&new).await.expect("probe the new address");
    ripdl_core::refresh::check_compatible(&dest, &st, &info)
        .expect("same file behind a fresh token");
    let kept = ripdl_core::refresh::kept_bytes(&st);
    assert!(kept > 0, "pause must have left durable progress");
    ripdl_core::refresh::retarget(&mut st, &new, info.etag.clone(), info.total);
    ripdl_core::refresh::save(&dest, &st).expect("sidecar rewritten");

    let seen_before = state.ranges_seen.lock().unwrap().len();
    let resumed_dest = engine
        .download(&new, Some(tmp.clone()), CancellationToken::new(), move |_p| {})
        .await
        .expect("resume from the refreshed address");
    assert_eq!(resumed_dest, dest, "must continue the same file");
    verify(&dest, 4 * MIB);

    let seen = state.ranges_seen.lock().unwrap().clone();
    let resumed = &seen[seen_before..];
    assert_eq!(resumed.len(), 8, "all 8 segments must finish, saw {seen:?}");
    for (k, r) in resumed.iter().enumerate() {
        let (a, _) = r.split_once('-').expect("range format");
        let start: u64 = a.parse().expect("range start");
        assert_ne!(
            start % 524_288,
            0,
            "segment {k} must resume mid-segment after the refresh, saw {r}"
        );
    }
    let names = dir_names(&dest.parent().unwrap());
    assert!(
        !names.iter().any(|n| n.ends_with(".ripdl.json")),
        "sidecar must be removed on success: {names:?}"
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
        content_disposition: None,
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
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

/// A check to buy drinks for: every request (probe + segments) carries the
/// configured headers, and a Content-Disposition filename override beats the
/// URL's last segment.
#[tokio::test]
async fn carries_headers_and_uses_cd_filename() {
    let state = Arc::new(ServerState {
        total: MIB,
        ranges: true,
        drop_after_bytes: 0,
        drop_times: 0,
        dropped: AtomicUsize::new(0),
        etag_version: AtomicUsize::new(100),
        block_pace: None,
        content_disposition: Some("attachment; filename*=UTF-8''ripdl%20%C3%A9tude.bin".to_string()),
        headers_seen: Mutex::new(Vec::new()),
        ranges_seen: Mutex::new(Vec::new()),
        peers_seen: Mutex::new(Vec::new()),
        quota_replies: 0,
        quota_served: AtomicUsize::new(0),
        hard_quota_replies: 0,
        hard_quota_served: AtomicUsize::new(0),
    });
    let url = spawn_server(Arc::clone(&state)).await;

    let mut headers = std::collections::HashMap::new();
    headers.insert("Referer".to_string(), "https://example.org/dir".to_string());
    headers.insert("User-Agent".to_string(), "ripdl-cli-test/2.0".to_string());
    headers.insert("Cookie".to_string(), "bin=31337".to_string());
    let engine = Engine::new(EngineConfig {
        max_segments: 8,
        min_segment_size: 128 * KIB,
        retry: RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(200),
        },
        headers,
        ..EngineConfig::default()
    })
    .expect("engine");

    let tmp = temp_dir("headers");
    let dest = engine
        .download(&url, Some(tmp), CancellationToken::new(), move |_p| {})
        .await
        .expect("download ok");

    // The RFC 5987 name wins over the URL's /file.bin.
    assert_eq!(
        dest.file_name().and_then(|n| n.to_str()),
        Some("ripdl étude.bin")
    );
    verify(&dest, MIB);

    let seen = state.headers_seen.lock().unwrap().clone();
    for wanted in [
        "referer: https://example.org/dir",
        "user-agent: ripdl-cli-test/2.0",
        "cookie: bin=31337",
    ] {
        assert!(
            seen.iter().any(|h| h == wanted),
            "header '{wanted}' missing, saw {seen:?}"
        );
    }
    // The built-in UA must yield to the user's.
    assert!(
        !seen.iter().any(|h| h.starts_with("user-agent: ripdl/")),
        "default UA leaked past the override, saw {seen:?}"
    );
}
