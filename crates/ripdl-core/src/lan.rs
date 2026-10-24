//! Same-network file transfer: a tiny HTTP drop box, a sender, and LAN
//! discovery.
//!
//! Why not a real HTTP stack: a LAN drop box needs exactly two verbs (upload
//! bytes, download bytes) plus two JSON listings, and every byte has to be
//! counted for progress. A hand-rolled HTTP/1.1 exchange over tokio keeps the
//! dependency surface unchanged (no new crates), lets the sender stream a
//! multi-GB file with an exact `Content-Length`, and makes the receiver's
//! limits explicit. Browsers are the other client, so the page is a
//! self-contained HTML+JS file that posts each picked file as a raw body.
//!
//! Security model: the receiver listens on the LAN and requires a per-device
//! code (shown in the app) for uploads and listings, unless the user turns on
//! "accept without a code" for a trusted home network. Uploaded names are
//! sanitized to a single file name inside the inbox; nothing outside it can be
//! read or written.

use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::watch;

use crate::error::{Error, Result};
use crate::names::{sanitize_component, suffixed};

/// Default receive port (bound on all interfaces when receiving is on).
pub const DEFAULT_PORT: u16 = 8463;
/// UDP port used to answer "who is receiving on this LAN?" beacons.
pub const DISCOVERY_PORT: u16 = 8464;

const DISCOVER_MAGIC: &[u8] = b"RIPDL-DISCOVER/1";
const HEAD_LIMIT: usize = 16 * 1024;
const COPY_CHUNK: usize = 256 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One file sitting in the receive folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanFile {
    pub name: String,
    pub size: u64,
    /// Unix seconds (0 when the filesystem does not report a time).
    pub modified: i64,
    /// Still arriving: `size` is how much has landed so far.
    #[serde(default)]
    pub partial: bool,
}

/// A receiver discovered on the local network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanPeer {
    /// Base URL, e.g. `http://192.168.1.8:8463`.
    pub url: String,
    /// Device name shown to the user.
    pub name: String,
    pub port: u16,
    /// Receiver accepts uploads without a code (trusted-network mode).
    pub open: bool,
}

/// Receiver settings. `host` is `0.0.0.0` in production and loopback in tests.
#[derive(Debug, Clone)]
pub struct LanConfig {
    pub host: IpAddr,
    pub port: u16,
    pub dir: PathBuf,
    pub code: String,
    pub open: bool,
    pub name: String,
}

struct ServerState {
    dir: PathBuf,
    code: String,
    open: bool,
    peer: LanPeer,
    version: String,
}

impl ServerState {
    fn code_ok(&self, query: &[(String, String)]) -> bool {
        if self.open {
            return true;
        }
        query
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| constant_eq(v, &self.code))
            .unwrap_or(false)
    }
}

/// A running receiver. Dropping it closes the listener and the discovery
/// responder (the daemon holds it until receiving is turned off or the port
/// changes).
pub struct LanServer {
    url: String,
    port: u16,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl LanServer {
    pub async fn start(cfg: LanConfig) -> Result<Self> {
        tokio::fs::create_dir_all(&cfg.dir).await?;
        let listener = TcpListener::bind((cfg.host, cfg.port)).await.map_err(|e| {
            Error::State(format!(
                "cannot listen on {}:{} ({e})",
                cfg.host, cfg.port
            ))
        })?;
        let port = listener.local_addr()?.port();
        let ip = if cfg.host.is_unspecified() {
            lan_ipv4().unwrap_or(Ipv4Addr::LOCALHOST)
        } else {
            match cfg.host {
                IpAddr::V4(v) => v,
                IpAddr::V6(_) => Ipv4Addr::LOCALHOST,
            }
        };
        let state = Arc::new(ServerState {
            dir: cfg.dir.clone(),
            code: cfg.code.clone(),
            open: cfg.open,
            peer: LanPeer {
                url: format!("http://{ip}:{port}"),
                name: cfg.name.clone(),
                port,
                open: cfg.open,
            },
            version: env!("CARGO_PKG_VERSION").to_string(),
        });
        let (stop, _) = watch::channel(false);
        let mut tasks = Vec::new();

        // TCP accept loop. One task per connection: an upload must never
        // block a browser's listing request.
        {
            let state = Arc::clone(&state);
            let mut rx = stop.subscribe();
            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = rx.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _addr)) = accepted else { continue };
                            let state = Arc::clone(&state);
                            tokio::spawn(async move {
                                let _ = handle_conn(stream, state).await;
                            });
                        }
                    }
                }
            }));
        }

        // Discovery responder. Best effort: a second ripdl on the same machine
        // cannot bind the port, and the sender side keeps working anyway.
        {
            let state = Arc::clone(&state);
            let mut rx = stop.subscribe();
            tasks.push(tokio::spawn(async move {
                let Ok(sock) = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).await else {
                    return;
                };
                let mut buf = [0u8; 64];
                loop {
                    tokio::select! {
                        _ = rx.changed() => break,
                        got = sock.recv_from(&mut buf) => {
                            let Ok((n, from)) = got else { continue };
                            if &buf[..n] != DISCOVER_MAGIC {
                                continue;
                            }
                            if let Ok(body) = serde_json::to_vec(&state.peer) {
                                let _ = sock.send_to(&body, from).await;
                            }
                        }
                    }
                }
            }));
        }

        Ok(Self {
            url: state.peer.url.clone(),
            port,
            stop,
            tasks,
        })
    }

    /// Base URL other devices use (`http://<lan ip>:<port>`).
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for LanServer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP/1.1, the small subset the page and the sender need
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    content_length: u64,
    /// False when the client sent no `Content-Length` at all (chunked or
    /// close-delimited bodies are not supported: an upload must declare its
    /// size so the receiver can verify every byte arrived).
    has_content_length: bool,
    leftover: Vec<u8>,
}

async fn read_head<R: AsyncRead + Unpin>(rd: &mut R) -> Result<Request> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        if buf.len() > HEAD_LIMIT {
            return Err(Error::State("request header too large".into()));
        }
        let n = tokio::time::timeout(IO_TIMEOUT, rd.read(&mut chunk))
            .await
            .map_err(|_| Error::State("timed out reading request".into()))??;
        if n == 0 {
            return Err(Error::State("connection closed before headers".into()));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let leftover = buf[header_end..].to_vec();
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| Error::State("empty request".into()))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target, Vec::new()),
    };
    let mut content_length = 0u64;
    let mut has_content_length = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            has_content_length = true;
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    Ok(Request {
        method,
        path,
        query,
        content_length,
        has_content_length,
        leftover,
    })
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

async fn respond<W: AsyncWrite + Unpin>(
    wr: &mut W,
    status: u16,
    reason: &str,
    content_type: &str,
    extra: &str,
    body: &[u8],
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n{extra}\r\n",
        body.len()
    );
    wr.write_all(head.as_bytes()).await?;
    wr.write_all(body).await?;
    wr.flush().await?;
    Ok(())
}

async fn respond_json<W: AsyncWrite + Unpin>(wr: &mut W, status: u16, reason: &str, body: &[u8]) -> Result<()> {
    respond(wr, status, reason, "application/json; charset=utf-8", "", body).await
}

async fn handle_conn(mut stream: TcpStream, state: Arc<ServerState>) -> Result<()> {
    let (mut rd, mut wr) = stream.split();
    let Request {
        method,
        path,
        query,
        content_length,
        has_content_length,
        mut leftover,
    } = read_head(&mut rd).await?;
    match (method.as_str(), path.as_str()) {
        ("GET", "/") => {
            let page = index_html(&state);
            respond(&mut wr, 200, "OK", "text/html; charset=utf-8", "", page.as_bytes()).await
        }
        ("GET", "/api/info") => {
            let body = serde_json::json!({
                "name": state.peer.name,
                "version": state.version,
                "port": state.peer.port,
                "open": state.open,
                "inbox": state.dir.to_string_lossy(),
            });
            respond_json(&mut wr, 200, "OK", body.to_string().as_bytes()).await
        }
        ("GET", "/api/check") => {
            if state.code_ok(&query) {
                respond_json(&mut wr, 200, "OK", b"{\"ok\":true}").await
            } else {
                respond_json(&mut wr, 403, "Forbidden", b"{\"error\":\"code\"}").await
            }
        }
        ("GET", "/api/files") => {
            if !state.code_ok(&query) {
                return respond_json(&mut wr, 403, "Forbidden", b"{\"error\":\"code\"}").await;
            }
            let files = files(&state.dir).await;
            let body = serde_json::to_vec(&files).unwrap_or_else(|_| b"[]".to_vec());
            respond_json(&mut wr, 200, "OK", &body).await
        }
        ("POST", "/api/send") => {
            if !state.code_ok(&query) {
                return respond_json(&mut wr, 403, "Forbidden", b"{\"error\":\"code\"}").await;
            }
            if !has_content_length {
                return respond_json(
                    &mut wr,
                    411,
                    "Length Required",
                    b"{\"error\":\"content-length required\"}",
                )
                .await;
            }
            let raw_name = query
                .iter()
                .find(|(k, _)| k == "name")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let name = sanitize_name(&raw_name);
            // In-flight uploads go to a token-named part file: two people
            // dropping the same file name at the same time must never write
            // into each other's bytes. The final name is chosen after the
            // body lands, so a collision at that point just becomes "x (1)".
            let token = part_token();
            let part = state.dir.join(format!(".part-{token}-{name}"));
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .await?;
            let mut written = 0u64;
            let mut buf = vec![0u8; COPY_CHUNK];
            if !leftover.is_empty() {
                let take = leftover.len().min(content_length as usize);
                file.write_all(&leftover[..take]).await?;
                written += take as u64;
                leftover.clear();
            }
            while written < content_length {
                let want = (content_length - written).min(COPY_CHUNK as u64) as usize;
                let n = tokio::time::timeout(IO_TIMEOUT, rd.read(&mut buf[..want]))
                    .await
                    .map_err(|_| Error::State("timed out receiving file".into()))??;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).await?;
                written += n as u64;
            }
            file.flush().await?;
            drop(file);
            if written != content_length {
                let _ = tokio::fs::remove_file(&part).await;
                return respond_json(&mut wr, 400, "Bad Request", b"{\"error\":\"truncated\"}").await;
            }
            let final_path = place_finished(&state.dir, &part, &name).await?;
            let body = serde_json::json!({
                "name": final_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                "bytes": written,
            });
            respond_json(&mut wr, 200, "OK", body.to_string().as_bytes()).await
        }
        ("GET", path) if path.starts_with("/files/") => {
            let raw = &path["/files/".len()..];
            let name = percent_decode(raw);
            let name = sanitize_name(&name);
            if name.is_empty() {
                return respond_json(&mut wr, 404, "Not Found", b"{\"error\":\"name\"}").await;
            }
            let served = state.dir.join(&name);
            let Ok(meta) = tokio::fs::metadata(&served).await else {
                return respond_json(&mut wr, 404, "Not Found", b"{\"error\":\"missing\"}").await;
            };
            if !meta.is_file() {
                return respond_json(&mut wr, 404, "Not Found", b"{\"error\":\"missing\"}").await;
            }
            if !state.code_ok(&query) {
                return respond_json(&mut wr, 403, "Forbidden", b"{\"error\":\"code\"}").await;
            }
            let disposition = format!(
                "attachment; filename*=UTF-8''{}",
                percent_encode(&name)
            );
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Disposition: {disposition}\r\nConnection: close\r\n\r\n",
                meta.len()
            );
            wr.write_all(head.as_bytes()).await?;
            let mut file = tokio::fs::File::open(&served).await?;
            let mut buf = vec![0u8; COPY_CHUNK];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                wr.write_all(&buf[..n]).await?;
            }
            wr.flush().await?;
            Ok(())
        }
        _ => respond_json(&mut wr, 404, "Not Found", b"{\"error\":\"route\"}").await,
    }
}

/// One file name, no directories, no Windows-reserved characters: the shared
/// helper with the inbox's fallback name.
fn sanitize_name(raw: &str) -> String {
    sanitize_component(raw, "received.bin")
}

/// Move a finished body to a free name inside `dir`. Claiming the name has
/// to be atomic: two uploads that finish at the same instant can both see
/// `x.bin` free, and a plain rename would silently drop one of them.
async fn place_finished(dir: &Path, part: &Path, name: &str) -> Result<PathBuf> {
    for n in 0..1000u32 {
        let candidate = dir.join(suffixed(name, n));
        // A hard link either creates the name with the finished content or
        // fails because it is taken - no window where an empty file shows up.
        match tokio::fs::hard_link(part, &candidate).await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(part).await;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // Filesystems without hard links (exFAT/FAT32 inbox on a USB
                // stick): claim the name exclusively, then move the body over
                // the claim we own.
                match tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&candidate)
                    .await
                {
                    Ok(_) => {
                        tokio::fs::rename(part, &candidate).await?;
                        return Ok(candidate);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Err(Error::State(
        "no free file name in the receive folder after 1000 tries".into(),
    ))
}

fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Eight hex chars for a part-file name. Time + a process-wide counter is
/// enough: the only requirement is that two concurrent uploads never pick
/// the same in-flight name.
fn part_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mix = nanos
        ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (std::process::id() as u64);
    format!("{:08x}", mix as u32)
}

// ---------------------------------------------------------------------------
// Sender
// ---------------------------------------------------------------------------

/// Where to send one file.
#[derive(Debug, Clone)]
pub struct SendConfig {
    /// Receiver base URL (`http://host:port` or bare `host:port`).
    pub url: String,
    /// Receiver code (ignored by an open receiver).
    pub code: String,
    pub path: PathBuf,
}

/// Upload one file, reporting `(sent, total)` as the bytes leave the disk.
/// Returns the name the receiver stored it under.
pub async fn send_file(
    cfg: &SendConfig,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<String> {
    let meta = tokio::fs::metadata(&cfg.path).await?;
    if !meta.is_file() {
        return Err(Error::State(format!(
            "not a file: {}",
            cfg.path.display()
        )));
    }
    let total = meta.len();
    let name = cfg
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::State("file has no name".into()))?;
    let (host, port) = parse_host_port(&cfg.url)?;
    let addr = tokio::net::lookup_host((host.as_str(), port))
        .await?
        .next()
        .ok_or_else(|| Error::State(format!("cannot resolve {host}:{port}")))?;
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::State(format!("timed out connecting to {host}:{port}")))??;
    let _ = stream.set_nodelay(true);
    let (mut rd, mut wr) = stream.into_split();

    // Ask first (on its own connection). A wrong code must fail before a
    // multi-GB body starts streaming into a receiver that will reject it.
    check_code(&host, port, &cfg.code).await?;

    let head = format!(
        "POST /api/send?name={}&code={} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/octet-stream\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
        percent_encode(&name),
        percent_encode(&cfg.code)
    );
    wr.write_all(head.as_bytes()).await?;

    let mut file = tokio::fs::File::open(&cfg.path).await?;
    let mut buf = vec![0u8; COPY_CHUNK];
    let mut sent = 0u64;
    let mut write_err = None;
    on_progress(0, total);
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if let Err(e) = wr.write_all(&buf[..n]).await {
            write_err = Some(e);
            break;
        }
        sent += n as u64;
        on_progress(sent, total);
    }
    let _ = wr.flush().await;

    // Response: a JSON object with the stored name, or an error status.
    let (status, body) = match read_response(&mut rd).await {
        Ok(pair) => pair,
        Err(read_err) => {
            return Err(match write_err {
                Some(e) => Error::from(e),
                None => read_err,
            })
        }
    };
    if status != 200 {
        return Err(Error::State(if body.is_empty() {
            format!("receiver answered {status}")
        } else {
            format!("receiver answered {status}: {body}")
        }));
    }
    let stored = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .unwrap_or(name);
    Ok(stored)
}

/// Ask the receiver whether the code is accepted, on a throwaway connection.
async fn check_code(host: &str, port: u16, code: &str) -> Result<()> {
    let addr = tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| Error::State(format!("cannot resolve {host}:{port}")))?;
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::State(format!("timed out connecting to {host}:{port}")))??;
    let (mut rd, mut wr) = stream.into_split();
    let head = format!(
        "GET /api/check?code={} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n",
        percent_encode(code)
    );
    wr.write_all(head.as_bytes()).await?;
    wr.flush().await?;
    let (status, body) = read_response(&mut rd).await?;
    if status != 200 {
        return Err(Error::State(if body.is_empty() {
            format!("receiver answered {status} (wrong code?)")
        } else {
            format!("receiver answered {status}: {body}")
        }));
    }
    Ok(())
}

async fn read_response<R: AsyncRead + Unpin>(rd: &mut R) -> Result<(u16, String)> {
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    let end = loop {
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        if buf.len() > HEAD_LIMIT {
            return Err(Error::State("response header too large".into()));
        }
        let n = tokio::time::timeout(IO_TIMEOUT, rd.read(&mut chunk))
            .await
            .map_err(|_| Error::State("timed out waiting for the receiver".into()))??;
        if n == 0 {
            return Err(Error::State("receiver closed the connection".into()));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| Error::State("malformed receiver response".into()))?;
    // Content-Length is all we need: the JSON body is tiny.
    let mut len = 0usize;
    for line in head.split("\r\n").skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[end..].to_vec();
    while body.len() < len {
        let n = tokio::time::timeout(IO_TIMEOUT, rd.read(&mut chunk))
            .await
            .map_err(|_| Error::State("timed out reading response".into()))??;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn parse_host_port(url: &str) -> Result<(String, u16)> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed
        .strip_prefix("http://")
        .unwrap_or(trimmed);
    if trimmed.starts_with("https://") {
        return Err(Error::State(
            "LAN transfers use http:// (this is a local-network tool)".into(),
        ));
    }
    let trimmed = trimmed.split('/').next().unwrap_or(trimmed);
    match trimmed.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Ok((
            host.trim_matches(['[', ']']).to_string(),
            port.parse().map_err(|_| Error::State("bad port".into()))?,
        )),
        _ => Ok((trimmed.to_string(), DEFAULT_PORT)),
    }
}

// ---------------------------------------------------------------------------
// Discovery + inbox
// ---------------------------------------------------------------------------

/// Ask the LAN for receivers. Returns unique peers; empty when nobody answers
/// (firewall, no receiver, or the beacon was dropped).
pub async fn discover(timeout: Duration) -> Vec<LanPeer> {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)).await else {
        return Vec::new();
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.send_to(DISCOVER_MAGIC, ("255.255.255.255", DISCOVERY_PORT)).await;
    let mut out: Vec<LanPeer> = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _from))) => {
                if let Ok(peer) = serde_json::from_slice::<LanPeer>(&buf[..n]) {
                    if !out.iter().any(|p| p.url == peer.url) {
                        out.push(peer);
                    }
                }
            }
            _ => break,
        }
    }
    out
}

/// Files in the receive folder, newest first. In-flight uploads appear while
/// their body is still arriving (`.part-<token>-<name>`), so a multi-GB copy
/// is visible instead of looking like nothing happened.
pub async fn files(dir: &Path) -> Vec<LanFile> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let raw = entry.file_name().to_string_lossy().into_owned();
        let (name, partial) = match raw.strip_prefix(".part-") {
            Some(rest) if rest.len() > 9 && rest.as_bytes()[8] == b'-' => {
                (rest[9..].to_string(), true)
            }
            _ => {
                if raw.ends_with(".part") {
                    continue;
                }
                (raw.clone(), false)
            }
        };
        out.push(LanFile {
            name,
            size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            partial,
        });
    }
    // Finished files above anything still arriving, then newest first.
    out.sort_by(|a, b| {
        a.partial
            .cmp(&b.partial)
            .then(b.modified.cmp(&a.modified))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Primary LAN IPv4: the default-route interface first, then any up,
/// non-loopback interface.
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    let mut fallback = None;
    for iface in netdev::get_interfaces() {
        if !iface.is_up() || iface.is_loopback() {
            continue;
        }
        let Some(&ip) = iface.ipv4_addrs().first() else {
            continue;
        };
        if iface.default {
            return Some(ip);
        }
        if fallback.is_none() {
            fallback = Some(ip);
        }
    }
    fallback
}

/// A short, easy-to-read code. Ambiguous characters (0/O, 1/I) are skipped so
/// it can be read off a screen and typed on a phone.
pub fn generate_code() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    seed ^= std::process::id() as u64;
    let mut out = String::with_capacity(6);
    for _ in 0..6 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let idx = ((seed >> 33) as usize) % ALPHABET.len();
        out.push(ALPHABET[idx] as char);
    }
    out
}

fn index_html(state: &ServerState) -> String {
    INLINE_PAGE
        .replace("__NAME__", &html_escape(&state.peer.name))
        .replace("__VERSION__", &html_escape(&state.version))
        .replace("__URL__", &html_escape(&state.peer.url))
        .replace(
            "__OPEN__",
            if state.open { "true" } else { "false" },
        )
        .replace(
            "__CODE_HINT__",
            if state.open {
                "This device accepts files from the local network without a code."
            } else {
                "Enter the code shown in ripdl on this device."
            },
        )
}

/// The drop-box page. Self-contained (no CDN, no fonts, no telemetry): pick
/// files, post each as a raw body, watch the bar, download what arrived.
const INLINE_PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ripdl - __NAME__</title>
<style>
 :root { color-scheme: light dark; }
 body { font: 15px/1.45 "Segoe UI", system-ui, sans-serif; margin: 0 auto; padding: 24px; max-width: 760px; }
 h1 { font-size: 20px; margin: 0 0 4px; }
 .muted { color: #66717b; font-size: 13px; }
 .card { border: 1px solid #d3d8de; border-radius: 8px; padding: 16px; margin: 18px 0; }
 .row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
 input[type=password], input[type=text] { font: inherit; padding: 7px 9px; border: 1px solid #c3c9d0; border-radius: 6px; width: 170px; }
 input[type=file] { font: inherit; }
 button { font: inherit; padding: 8px 16px; border: 0; border-radius: 6px; background: #1f6fb0; color: #fff; cursor: pointer; }
 button:disabled { background: #9db4c9; cursor: default; }
 #refresh { background: #5b6770; padding: 4px 10px; }
 .bar { height: 8px; border-radius: 4px; background: #e7ebef; overflow: hidden; margin-top: 10px; }
 .bar > i { display: block; height: 100%; width: 0; background: #1f6fb0; }
 ul { list-style: none; padding: 0; margin: 8px 0 0; }
 li { display: flex; justify-content: space-between; gap: 12px; padding: 6px 0; border-top: 1px solid #eceff2; }
 a { color: #1f6fb0; }
 .err { color: #b02a2a; min-height: 18px; }
</style></head>
<body>
<h1>__NAME__</h1>
<div class="muted">ripdl __VERSION__ - receive files from this local network</div>
<div class="card">
  <div class="row"><strong>Send a file</strong></div>
  <p class="muted">__CODE_HINT__</p>
  <div class="row">
    <input id="code" type="password" placeholder="code" autocomplete="off">
    <input id="file" type="file" multiple>
    <button id="send">Send</button>
  </div>
  <div class="bar"><i id="bar"></i></div>
  <div id="status" class="muted"></div>
  <div id="err" class="err"></div>
</div>
<div class="card">
  <div class="row"><strong>Received files</strong><button id="refresh">Refresh</button></div>
  <ul id="files"><li class="muted">...</li></ul>
  <div class="muted">Address: <a href="/">__URL__</a></div>
</div>
<script>
const $ = (s) => document.querySelector(s);
const openMode = __OPEN__;
function fmt(n) {
  return n >= 1e9 ? (n / 1e9).toFixed(2) + " GB"
       : n >= 1e6 ? (n / 1e6).toFixed(1) + " MB"
       : n >= 1e3 ? Math.round(n / 1e3) + " KB"
       : n + " B";
}
function api(path) {
  const c = $("#code").value.trim();
  return path + (path.includes("?") ? "&" : "?") + "code=" + encodeURIComponent(c);
}
function esc(s) {
  return String(s).replace(/[<>&"]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;" })[c]);
}
async function loadFiles() {
  try {
    const r = await fetch(api("/api/files"));
    if (r.status === 403) {
      $("#files").innerHTML = '<li class="muted">enter the code to list files</li>';
      return;
    }
    const items = await r.json();
    $("#files").innerHTML = items.length
      ? items.map((f) => '<li><span>' + esc(f.name) + '</span><span><span class="muted">'
          + fmt(f.size) + '</span> <a href="' + esc(api("/files/" + encodeURIComponent(f.name)))
          + '">download</a></span></li>').join("")
      : '<li class="muted">nothing received yet</li>';
  } catch (e) {
    $("#files").innerHTML = '<li class="err">' + esc(e) + '</li>';
  }
}
function upload(file) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", api("/api/send") + "&name=" + encodeURIComponent(file.name));
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) {
        $("#bar").style.width = (e.loaded / e.total * 100) + "%";
        $("#status").textContent = file.name + " - " + fmt(e.loaded) + " / " + fmt(e.total);
      }
    };
    xhr.onload = () => (xhr.status === 200 ? resolve() : reject(xhr.responseText || ("HTTP " + xhr.status)));
    xhr.onerror = () => reject("network error");
    xhr.send(file);
  });
}
$("#send").onclick = async () => {
  const files = Array.from($("#file").files);
  if (!files.length) { $("#err").textContent = "pick a file first"; return; }
  $("#send").disabled = true;
  $("#err").textContent = "";
  try {
    for (const f of files) await upload(f);
    $("#status").textContent = "sent " + files.length + " file(s)";
    loadFiles();
  } catch (e) {
    $("#err").textContent = "send failed: " + e;
  } finally {
    $("#send").disabled = false;
    $("#bar").style.width = "0%";
    $("#file").value = "";
  }
};
$("#refresh").onclick = loadFiles;
$("#code").addEventListener("change", loadFiles);
loadFiles();
</script>
</body></html>"##;
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ripdl-lan-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn start(dir: &Path, open: bool, code: &str) -> LanServer {
        LanServer::start(LanConfig {
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            dir: dir.to_path_buf(),
            code: code.to_string(),
            open,
            name: "testbox".into(),
        })
        .await
        .expect("start receiver")
    }

    #[tokio::test]
    async fn send_receive_roundtrip_with_progress() {
        let dir = temp_dir("roundtrip");
        let src_dir = temp_dir("roundtrip-src");
        let server = start(&dir, false, "ABCDEF").await;
        let src = src_dir.join("payload.bin");
        let data: Vec<u8> = (0..(3 * 1024 * 1024 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        tokio::fs::write(&src, &data).await.unwrap();

        let mut last = 0u64;
        let stored = send_file(
            &SendConfig {
                url: server.url().to_string(),
                code: "ABCDEF".into(),
                path: src.clone(),
            },
            |sent, total| {
                assert!(sent <= total);
                assert!(sent >= last);
                last = sent;
            },
        )
        .await
        .expect("send");
        assert_eq!(stored, "payload.bin");
        assert_eq!(last, data.len() as u64);

        let got = tokio::fs::read(dir.join("payload.bin")).await.unwrap();
        assert_eq!(got, data);
    }

    #[tokio::test]
    async fn wrong_code_is_rejected_and_open_mode_skips_it() {
        let dir = temp_dir("code");
        let src_dir = temp_dir("code-src");
        let server = start(&dir, false, "ABCDEF").await;
        let src = src_dir.join("a.txt");
        tokio::fs::write(&src, b"hello").await.unwrap();

        let err = send_file(
            &SendConfig {
                url: server.url().to_string(),
                code: "WRONG1".into(),
                path: src.clone(),
            },
            |_, _| {},
        )
        .await
        .expect_err("wrong code must fail");
        assert!(err.to_string().contains("403"), "{err}");
        assert!(!tokio::fs::try_exists(dir.join("a.txt")).await.unwrap());

        let open_dir = temp_dir("open");
        let open = start(&open_dir, true, "ABCDEF").await;
        let stored = send_file(
            &SendConfig {
                url: open.url().to_string(),
                code: String::new(),
                path: src,
            },
            |_, _| {},
        )
        .await
        .expect("open receiver accepts");
        assert_eq!(stored, "a.txt");
    }

    #[tokio::test]
    async fn names_are_sanitized_and_never_overwrite() {
        let dir = temp_dir("names");
        let src_dir = temp_dir("names-src");
        let server = start(&dir, true, "x").await;
        let src = src_dir.join("payload.bin");
        tokio::fs::write(&src, b"first").await.unwrap();
        let cfg = SendConfig {
            url: server.url().to_string(),
            code: String::new(),
            path: src.clone(),
        };
        send_file(&cfg, |_, _| {}).await.unwrap();
        send_file(&cfg, |_, _| {}).await.unwrap();

        assert!(tokio::fs::try_exists(dir.join("payload.bin")).await.unwrap());
        assert!(tokio::fs::try_exists(dir.join("payload (1).bin")).await.unwrap());
        // Path components never escape the inbox.
        assert_eq!(sanitize_name("../../evil.sh"), "evil.sh");
        assert_eq!(sanitize_name("..\\..\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_name("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_name(".."), "received.bin");
        assert_eq!(sanitize_name("bad:name?.txt"), "bad_name_.txt");
        assert_eq!(sanitize_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_name("nul"), "_nul");
        assert_eq!(sanitize_name("console.txt"), "console.txt");
    }

    #[tokio::test]
    async fn parallel_same_name_uploads_stay_intact() {
        let dir = temp_dir("parallel");
        let src_dir = temp_dir("parallel-src");
        let server = start(&dir, true, "x").await;
        let src = src_dir.join("same.bin");
        let data: Vec<u8> = (0..(2 * 1024 * 1024)).map(|i| (i % 253) as u8).collect();
        tokio::fs::write(&src, &data).await.unwrap();
        let cfg = SendConfig {
            url: server.url().to_string(),
            code: String::new(),
            path: src,
        };
        let (a, b) = tokio::join!(send_file(&cfg, |_, _| {}), send_file(&cfg, |_, _| {}));
        a.unwrap();
        b.unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["same (1).bin".to_string(), "same.bin".to_string()]);
        for name in names {
            let bytes = tokio::fs::read(dir.join(&name)).await.unwrap();
            assert_eq!(bytes, data, "{name} is corrupt");
        }
    }

    #[tokio::test]
    async fn upload_without_content_length_is_rejected() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = temp_dir("nolen");
        let server = start(&dir, false, "CODE99").await;
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", server.port()))
            .await
            .unwrap();
        sock.write_all(b"POST /api/send?name=x.bin&code=CODE99 HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf).to_string();
        assert!(text.starts_with("HTTP/1.1 411"), "{text}");
        assert!(!tokio::fs::try_exists(dir.join("x.bin")).await.unwrap());
    }

    #[tokio::test]
    async fn page_info_and_download_endpoints_answer() {
        let dir = temp_dir("http");
        let server = start(&dir, false, "CODE99").await;
        tokio::fs::write(dir.join("note.txt"), b"download me").await.unwrap();
        let port = server.port();

        async fn get(port: u16, path: &str) -> (String, String) {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf).to_string();
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            (head.to_string(), body.to_string())
        }

        let (head, body) = get(port, "/").await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(body.contains("Send a file"));
        // The page must not leak the code itself.
        assert!(!body.contains("CODE99"));

        let (head, body) = get(port, "/api/info").await;
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(body.contains("\"name\""));

        let (head, _) = get(port, "/api/files").await;
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");

        let (head, body) = get(port, "/api/files?code=CODE99").await;
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(body.contains("note.txt"));

        let (head, _) = get(port, "/files/note.txt").await;
        assert!(head.starts_with("HTTP/1.1 403"));

        let (head, body) = get(port, "/files/note.txt?code=CODE99").await;
        assert!(head.starts_with("HTTP/1.1 200"));
        assert_eq!(body, "download me");
    }

    #[tokio::test]
    async fn partial_uploads_are_listed_as_in_flight() {
        let dir = temp_dir("partial");
        tokio::fs::write(dir.join(".part-0a1b2c3d-big.iso"), b"1234")
            .await
            .unwrap();
        tokio::fs::write(dir.join("done.bin"), b"123456").await.unwrap();
        tokio::fs::write(dir.join("legacy.part"), b"x").await.unwrap();
        let listed = files(&dir).await;
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert_eq!(listed[0].name, "done.bin");
        assert!(!listed[0].partial);
        assert_eq!(listed[1].name, "big.iso");
        assert!(listed[1].partial);
        assert_eq!(listed[1].size, 4);
    }

    #[test]
    fn codes_avoid_ambiguous_characters() {
        for _ in 0..50 {
            let code = generate_code();
            assert_eq!(code.len(), 6);
            assert!(!code.contains(['O', '0', 'I', '1']), "{code}");
        }
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            parse_host_port("http://192.168.1.5:8463").unwrap(),
            ("192.168.1.5".to_string(), 8463)
        );
        assert_eq!(
            parse_host_port("192.168.1.5:9000/").unwrap(),
            ("192.168.1.5".to_string(), 9000)
        );
        assert_eq!(
            parse_host_port("mybox").unwrap(),
            ("mybox".to_string(), DEFAULT_PORT)
        );
        assert!(parse_host_port("https://x:1").is_err());
    }
}
