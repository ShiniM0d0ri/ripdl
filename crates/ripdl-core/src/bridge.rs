//! Native-messaging host: bridges a browser extension to the ripdl daemon.
//!
//! The browser launches us with the extension origin as `argv[1]` and talks
//! Chrome's native-messaging framing (u32 little-endian length + UTF-8 JSON)
//! over stdio. Each JSON message is the same protocol the CLI speaks to the
//! daemon — `{"id": "<caller-chosen>", "cmd": {...}}` →
//! `{"id": "<caller-chosen>", "reply": {...}}` — with one addition: ids are
//! strings here, and the bridge regenerates its own numeric RPC ids toward
//! the daemon.
//!
//! Design contract with the extension: a rejected/unreachable daemon yields
//! an `error` reply, and the extension then lets the browser download
//! normally. The host never blocks browsing because ripdl is missing.

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::rpc::{Command, Reply};

/// Chrome caps messages sent *by* the host at 1 MiB. Stay under it; a `tasks`
/// listing wider than this earns an in-band error, not a corrupt frame.
pub const HOST_TO_BROWSER_MAX: usize = 1024 * 1024;
/// Inbound frames are chunky by design (a page can hand us a huge link list),
/// but a frame header far beyond anything a real message carries is a bug.
pub const BROWSER_TO_HOST_MAX: usize = 16 * 1024 * 1024;

/// Stable unpacked-extension ID. `ext/manifest.json` embeds a pinned RSA
/// public key ("key" field), which overrides Chromium's default
/// load-path-hash ID derivation - loading the extension from any folder
/// gives this ID everywhere, so `register-host` knows it by default and
/// users never hit a silently-rejected native host after moving the folder.
pub const PINNED_EXT_ID: &str = "cimcjenbnacepkgebkmpkggjoahdmggc";

/// One request from the browser: its id is opaque and echoed back unchanged.
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeRequest {
    pub id: String,
    pub cmd: Command,
}

/// One response toward the browser: same protocol shape as `rpc::Response`,
/// but the caller's string id survives the trip through the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeResponse {
    pub id: String,
    pub reply: Reply,
}

/// Read one native-messaging frame. `Ok(None)` means clean EOF (the browser
/// closed the pipe); a truncated frame or an absurd length is a hard error.
pub fn read_frame(reader: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > BROWSER_TO_HOST_MAX {
        return Err(Error::State(format!("native frame too large: {len}")));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Write one native-messaging frame, refusing messages Chrome would reject.
pub fn write_frame(writer: &mut impl Write, msg: &[u8]) -> Result<()> {
    if msg.len() + 4 > HOST_TO_BROWSER_MAX {
        return Err(Error::State(format!(
            "native reply too large: {} bytes (cap {HOST_TO_BROWSER_MAX})",
            msg.len()
        )));
    }
    writer.write_all(&(msg.len() as u32).to_le_bytes())?;
    writer.write_all(msg)?;
    writer.flush()?;
    Ok(())
}

/// Connection behavior of the host bridge.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Daemon TCP port (same RPC the CLI uses).
    pub port: u16,
    /// Connect attempts per request before answering with an error reply.
    pub connect_attempts: u32,
    /// Delay between attempts.
    pub connect_delay: Duration,
    /// Spawn a sibling `ripdl-daemon` binary when nothing listens yet.
    pub spawn_daemon: bool,
    /// Read timeout on daemon replies (a stuck daemon must not wedge the
    /// browser's host process).
    pub rpc_timeout: Duration,
    /// Try the local-session transports (named pipe / unix socket) before
    /// TCP. Tests disable this: an unrelated live daemon on the build host
    /// owns the pipe and would answer for any mock TCP server otherwise.
    pub use_pipe: bool,
    /// Token presented first on every connection. `None` reads `rpc.token`
    /// from the data directory, which is what the installed host does.
    pub rpc_token: Option<String>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            port: crate::transport::rpc_port(),
            connect_attempts: 20,
            connect_delay: Duration::from_millis(100),
            spawn_daemon: true,
            rpc_timeout: Duration::from_secs(30),
            use_pipe: true,
            rpc_token: None,
        }
    }
}

/// Worker threads answering browser requests. A `propose` can wait up to 2 s
/// in the daemon, and a slow daemon can hold any request much longer; with a
/// single worker, every frame queued behind such a request stalled too.
const WORKERS: usize = 8;
/// Frames read ahead of the workers. Once this many are waiting the reader
/// stops pulling from the browser, which is the backpressure we want.
const QUEUE_DEPTH: usize = 64;

/// Serve the native-messaging loop on stdin/stdout until the pipe closes.
/// Per-request failures become `error` replies in-band; this function only
/// returns when stdin reaches EOF or the framing itself breaks.
///
/// Requests run concurrently on a small worker pool, so replies can arrive
/// out of order. Each one carries the caller's id, which is how the extension
/// matches it to its request.
pub fn serve(config: BridgeConfig) -> Result<()> {
    serve_with(config, std::io::stdin().lock(), std::io::stdout())
}

/// [`serve`] over any reader and writer. Returns once the reader reaches EOF
/// and every queued request has been answered.
pub fn serve_with<R, W>(config: BridgeConfig, mut input: R, output: W) -> Result<()>
where
    R: Read,
    W: Write + Send + 'static,
{
    let out = Arc::new(Mutex::new(output));
    let config = Arc::new(config);
    let rpc_seq = Arc::new(AtomicU64::new(0));
    let (tx, rx) = std::sync::mpsc::sync_channel::<NativeRequest>(QUEUE_DEPTH);
    let rx = Arc::new(Mutex::new(rx));

    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (rx, out, config, rpc_seq) =
                (rx.clone(), out.clone(), config.clone(), rpc_seq.clone());
            std::thread::spawn(move || answer_loop(&rx, &out, &config, &rpc_seq))
        })
        .collect();
    // Only the workers keep the queue alive. When they have all exited (the
    // browser went away), the reader's send fails instead of blocking forever.
    drop(rx);

    let result = read_loop(&mut input, &tx, &out);
    // Closing the queue lets the workers finish what is already read.
    drop(tx);
    for worker in workers {
        let _ = worker.join();
    }
    result
}

/// Read frames and queue them for the workers. Unparseable frames are
/// answered here, since they have no id a worker could answer under.
fn read_loop<R: Read, W: Write>(
    input: &mut R,
    tx: &SyncSender<NativeRequest>,
    out: &Mutex<W>,
) -> Result<()> {
    while let Some(frame) = read_frame(input)? {
        match serde_json::from_slice::<NativeRequest>(&frame) {
            Ok(msg) => {
                if tx.send(msg).is_err() {
                    return Err(Error::State("bridge workers stopped".to_string()));
                }
            }
            Err(e) => {
                // Unparseable frames still owe the caller an answer under
                // its own id - a newer extension's unknown Command variant
                // lands here, and a synthetic id would leave it waiting out
                // its full timeout on a reply it cannot correlate. The id
                // is recovered as the bare string it is on the wire.
                let id = serde_json::from_slice::<serde_json::Value>(&frame)
                    .ok()
                    .and_then(|v| {
                        v.get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "<parse>".to_string());
                let reply = NativeResponse {
                    id,
                    reply: Reply::Error {
                        message: format!("bad native request: {e}"),
                    },
                };
                send_reply(out, &reply)?;
            }
        }
    }
    Ok(())
}

/// One worker: take the next queued request, forward it, write the answer.
fn answer_loop<W: Write>(
    rx: &Mutex<Receiver<NativeRequest>>,
    out: &Mutex<W>,
    config: &BridgeConfig,
    rpc_seq: &AtomicU64,
) {
    loop {
        // The queue lock covers only the wait for the next request. The
        // forward itself runs unlocked so the other workers keep pulling.
        let next = match rx.lock() {
            Ok(queue) => queue.recv(),
            Err(poisoned) => poisoned.into_inner().recv(),
        };
        let Ok(msg) = next else { return };
        let reply = match forward_once(config, rpc_seq, msg.cmd) {
            Ok(r) => r,
            Err(e) => Reply::Error {
                message: e.to_string(),
            },
        };
        if send_reply(out, &NativeResponse { id: msg.id, reply }).is_err() {
            return;
        }
    }
}

/// Write one reply under the shared stdout lock, so frames from concurrent
/// workers never interleave. A reply too wide for one native frame becomes an
/// in-band error with the same id, so the caller still gets an answer.
fn send_reply<W: Write>(out: &Mutex<W>, resp: &NativeResponse) -> Result<()> {
    let mut json = serde_json::to_vec(resp)?;
    if json.len() + 4 > HOST_TO_BROWSER_MAX {
        let too_big = NativeResponse {
            id: resp.id.clone(),
            reply: Reply::Error {
                message: format!("reply too large for the browser ({} bytes)", json.len()),
            },
        };
        json = serde_json::to_vec(&too_big)?;
    }
    let mut writer = match out.lock() {
        Ok(w) => w,
        Err(poisoned) => poisoned.into_inner(),
    };
    write_frame(&mut *writer, &json)
}

/// Push one command to the daemon, spawning it if absent (once per call;
/// every later request just races the freshly spawned listener).
fn forward_once(config: &BridgeConfig, rpc_seq: &AtomicU64, cmd: Command) -> Result<Reply> {
    let mut spawned = false;
    for attempt in 0..config.connect_attempts {
        match talk(config, rpc_seq, &cmd) {
            Ok(reply) => return Ok(reply),
            Err(err) => {
                if config.spawn_daemon && !spawned && attempt == 0 {
                    spawned = true;
                    let _ = spawn_daemon_bg();
                }
                if attempt + 1 < config.connect_attempts {
                    std::thread::sleep(config.connect_delay);
                } else {
                    return Err(err);
                }
            }
        }
    }
    Err(Error::State("daemon unreachable".to_string()))
}

/// One connected daemon stream for the bridge's blocking IO. The named pipe
/// (Windows) / unix socket (elsewhere) is primary: no TCP handshake per
/// message, no port to race. TCP loopback stays as the fallback for
/// mixed-version dev setups.
trait RpcStream: std::io::Read + std::io::Write + Send + 'static {}
impl<T: std::io::Read + std::io::Write + Send + 'static> RpcStream for T {}

fn connect_daemon_sync(use_pipe: bool, port: u16) -> std::io::Result<Box<dyn RpcStream>> {
    #[cfg(windows)]
    // std's OpenOptions reaches a named-pipe server directly.
    if use_pipe {
        if let Ok(pipe) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(crate::transport::pipe_name())
        {
            return Ok(Box::new(pipe));
        }
    }
    #[cfg(not(windows))]
    if use_pipe {
        if let Ok(path) = crate::transport::unix_socket_path() {
            if let Ok(sock) = std::os::unix::net::UnixStream::connect(path) {
                return Ok(Box::new(sock));
            }
        }
    }
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    // Request/reply are small packets; Nagle would delay the daemon's reply.
    stream.set_nodelay(true)?;
    Ok(Box::new(stream))
}

fn talk(config: &BridgeConfig, rpc_seq: &AtomicU64, cmd: &Command) -> Result<Reply> {
    // Read the token per attempt: a daemon restarted under the browser writes
    // a fresh one, and the next attempt picks it up.
    let token = match &config.rpc_token {
        Some(token) => token.clone(),
        None => crate::transport::read_rpc_token()?,
    };
    let mut stream = connect_daemon_sync(config.use_pipe, config.port)?;
    // Build the request after the connection exists: a failed connect (spawn
    // warming up) must not clone the payload per retry attempt.
    let req = crate::rpc::Request {
        id: rpc_seq.fetch_add(1, Ordering::Relaxed) + 1,
        cmd: cmd.clone(),
    };

    // The auth line comes first on every connection, then the request. One
    // write, so the two cannot be split across packets.
    let mut line = crate::transport::auth_line(&token);
    line.push_str(&serde_json::to_string(&req)?);
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    // Read the reply on a worker thread and race it with the RPC deadline:
    // pipe handles have no std read timeout, and a hung daemon must not wed
    // the whole native-messaging host. One short-lived thread per message.
    let (tx, rx) = std::sync::mpsc::channel();
    let reply = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stream);
        let mut resp_line = Vec::new();
        let res = reader.read_until(b'\n', &mut resp_line).map(|_| resp_line);
        let _ = tx.send(res);
    });
    let resp_line = match rx.recv_timeout(config.rpc_timeout) {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            return Err(Error::State("daemon reply timed out".to_string()));
        }
    };
    drop(reply); // detached worker exits on its own after the send

    if resp_line.is_empty() {
        return Err(Error::State("daemon closed the connection".to_string()));
    }
    let resp: crate::rpc::Response = serde_json::from_slice(&resp_line)?;
    if resp.id != req.id {
        // Id 0 is the daemon's connection-level refusal (a token it rejects).
        if resp.id == 0 {
            return Err(Error::State(
                "daemon refused the connection (rpc token rejected or protocol mismatch)"
                    .to_string(),
            ));
        }
        return Err(Error::State(format!(
            "daemon reply id mismatch: sent {}, got {}",
            req.id, resp.id
        )));
    }
    Ok(resp.reply)
}

/// Start the sibling `ripdl-daemon` binary from next to the running exe,
/// detached and window-less. Best effort: failure is fine, the caller already
/// reports "daemon unreachable" after exhausting retries. Shared by the
/// native-messaging bridge and the GUI shell.
///
/// Rate-limited: second-guessing pollers (GUI) retry every second, and while
/// an old daemon is still shutting its ports down each retry would launch
/// another process that only binds and dies. One spawn attempt at most per
/// rate-limit window.
pub fn spawn_daemon_bg() -> Result<()> {
    const RESPAWN_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(3);
    static LAST_SPAWN: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    {
        let mut last = LAST_SPAWN.lock().unwrap();
        if let Some(t) = *last {
            if t.elapsed() < RESPAWN_COOLDOWN {
                return Err(Error::State("spawn rate-limited".to_string()));
            }
        }
        *last = Some(std::time::Instant::now());
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| Error::State("exe has no parent dir".to_string()))?;
    #[cfg(windows)]
    let daemon = dir.join("ripdl-daemon.exe");
    #[cfg(not(windows))]
    let daemon = dir.join("ripdl-daemon");
    if !daemon.exists() {
        return Err(Error::State(format!(
            "daemon binary not found: {}",
            daemon.display()
        )));
    }

    let mut cmd = std::process::Command::new(daemon);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: spawned from a browser-driven process, must not
        // flash consoles when Chrome starts/stops native hosts.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()?;
    Ok(())
}

/// Location hint used by `register-host`: where the native-messaging manifest
/// JSON belongs (under the daemon's data directory).
pub fn manifest_path(host_name: &str) -> Result<PathBuf> {
    let base = directories_and_localappdata()?;
    Ok(base.join("ripdl").join(format!("{host_name}.json")))
}

fn directories_and_localappdata() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| Error::State("%LOCALAPPDATA% not set".to_string()))
    }
    #[cfg(not(windows))]
    {
        // XDG config home (~/.config/ripdl) mirrors the manifest layout we
        // document for Linux.
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| Error::State("neither XDG_CONFIG_HOME nor HOME set".to_string()))?;
        Ok(PathBuf::from(home).join(".config"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_roundtrip() {
        let json = br#"{"hello": "world"}"#;
        let mut wire = Vec::new();
        write_frame(&mut wire, json).unwrap();

        let mut cursor = Cursor::new(wire);
        let got = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(got, json.to_vec());
        // EOF after the frame.
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn empty_stream_is_clean_eof() {
        let mut empty = Cursor::new(Vec::<u8>::new());
        assert!(read_frame(&mut empty).unwrap().is_none());
        // ...even after a write-side no-op.
        let mut sink = Vec::new();
        write_frame(&mut sink, b"{}").unwrap();
    }

    #[test]
    fn oversized_inbound_frame_is_rejected() {
        // Length header of u32::MAX with 2 junk bytes: the length check must
        // fire before any body allocation/read.
        let mut wire = Vec::new();
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        wire.extend_from_slice(&[0x7b, 0x7d]);
        let mut cursor = Cursor::new(wire);
        assert!(read_frame(&mut cursor).is_err());
    }

    #[test]
    fn oversized_outbound_frame_is_rejected() {
        let mut sink = Vec::new();
        let big = vec![0u8; HOST_TO_BROWSER_MAX];
        assert!(write_frame(&mut sink, &big).is_err());
        // Under the cap fits.
        let okmsg = vec![0u8; HOST_TO_BROWSER_MAX - 5];
        write_frame(&mut sink, &okmsg).unwrap();
    }

    /// `add` as the browser sends it, for the bridge tests.
    fn add_cmd(url: &str) -> Command {
        Command::Add {
            url: url.to_string(),
            dest_dir: None,
            headers: Default::default(),
            start_at: None,
            kind: Default::default(),
            sequential: Default::default(),
            only_files: Default::default(),
            on_duplicate: Default::default(),
        }
    }

    /// Cloneable in-memory sink for `serve_with` output.
    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Every native frame written to `out`, in arrival order.
    fn frames(out: &SharedBuf) -> Vec<NativeResponse> {
        let bytes = out.0.lock().unwrap().clone();
        let mut cursor = Cursor::new(bytes);
        let mut got = Vec::new();
        while let Some(frame) = read_frame(&mut cursor).unwrap() {
            got.push(serde_json::from_slice(&frame).unwrap());
        }
        got
    }

    /// Loopback stand-in for the daemon, one thread per connection like the
    /// real one. It checks the auth line first and refuses a wrong token the
    /// way the daemon does (id 0, then close). Otherwise it answers
    /// `Added { id: url }`, after 1.2 s when the URL contains "slow". Every
    /// request it answers is sent on the returned channel.
    fn fake_daemon(token: &'static str) -> (u16, std::sync::mpsc::Receiver<crate::rpc::Request>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(sock) = sock else { return };
                let seen_tx = seen_tx.clone();
                std::thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(sock);
                    let mut auth = Vec::new();
                    let _ = reader.read_until(b'\n', &mut auth);
                    let authed = std::str::from_utf8(&auth)
                        .is_ok_and(|l| crate::transport::auth_matches(l.trim_end(), token));
                    let mut line = Vec::new();
                    let _ = reader.read_until(b'\n', &mut line);
                    let reply = if !authed {
                        crate::rpc::Response {
                            id: 0,
                            reply: Reply::Error {
                                message: "unauthorized".to_string(),
                            },
                        }
                    } else {
                        let req: crate::rpc::Request = serde_json::from_slice(&line).unwrap();
                        let Command::Add { url, .. } = &req.cmd else {
                            panic!("fake daemon only answers add")
                        };
                        let url = url.clone();
                        if url.contains("slow") {
                            std::thread::sleep(Duration::from_millis(1200));
                        }
                        let resp = crate::rpc::Response {
                            id: req.id,
                            reply: Reply::Added { id: url },
                        };
                        let _ = seen_tx.send(req);
                        resp
                    };
                    let mut out = serde_json::to_string(&reply).unwrap();
                    out.push('\n');
                    let _ = reader.get_mut().write_all(out.as_bytes());
                });
            }
        });
        (port, seen_rx)
    }

    /// The bridge forwards one command over loopback: it opens with the auth
    /// line, assigns its own numeric rpc id, and returns the daemon's reply.
    #[test]
    fn forwards_to_daemon_and_back() {
        let (port, seen) = fake_daemon("tok");
        let config = BridgeConfig {
            port,
            spawn_daemon: false,
            connect_attempts: 1,
            use_pipe: false,
            rpc_token: Some("tok".to_string()),
            ..BridgeConfig::default()
        };
        let seq = AtomicU64::new(0);
        let reply = forward_once(&config, &seq, add_cmd("https://x/f.zip")).unwrap();
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            serde_json::json!({"type": "added", "id": "https://x/f.zip"})
        );
        let req = seen.recv().unwrap();
        assert_eq!(req.id, 1, "bridge assigns its own numeric rpc ids");
    }

    #[test]
    fn bridge_reports_a_refused_token_instead_of_a_mismatch() {
        let (port, _seen) = fake_daemon("right");
        let config = BridgeConfig {
            port,
            spawn_daemon: false,
            connect_attempts: 1,
            use_pipe: false,
            rpc_token: Some("wrong".to_string()),
            ..BridgeConfig::default()
        };
        let seq = AtomicU64::new(0);
        let err = forward_once(&config, &seq, Command::List).unwrap_err();
        assert!(err.to_string().contains("refused"), "got: {err}");
    }

    /// A slow request (the daemon holds it 1.2 s) must not hold up the fast
    /// one that was read after it. The old serial loop answered in order.
    #[test]
    fn slow_request_does_not_hold_up_fast_ones() {
        let (port, _seen) = fake_daemon("tok");
        let config = BridgeConfig {
            port,
            spawn_daemon: false,
            use_pipe: false,
            rpc_token: Some("tok".to_string()),
            ..BridgeConfig::default()
        };
        let mut input = Vec::new();
        for (id, url) in [
            ("slow", "https://x/slow.zip"),
            ("fast", "https://x/fast.zip"),
        ] {
            let req = NativeRequest {
                id: id.to_string(),
                cmd: add_cmd(url),
            };
            write_frame(&mut input, &serde_json::to_vec(&req).unwrap()).unwrap();
        }
        let out = SharedBuf::default();
        serve_with(config, Cursor::new(input), out.clone()).unwrap();
        let ids: Vec<String> = frames(&out).into_iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            ["fast", "slow"],
            "the fast reply must not wait behind the slow one"
        );
    }

    #[test]
    fn unparseable_frame_gets_an_inband_error_and_serving_continues() {
        let mut input = Vec::new();
        write_frame(&mut input, b"not json").unwrap();
        let config = BridgeConfig {
            spawn_daemon: false,
            use_pipe: false,
            ..BridgeConfig::default()
        };
        let out = SharedBuf::default();
        serve_with(config, Cursor::new(input), out.clone()).unwrap();
        let got = frames(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "<parse>");
        assert!(matches!(got[0].reply, Reply::Error { .. }));
    }

    /// A valid envelope carrying a command this host does not know (a newer
    /// extension talking to an older app) must answer under the caller's own
    /// id: a synthetic id leaves the extension waiting out its full timeout
    /// on a reply it cannot correlate, and every offer on the page stalls.
    #[test]
    fn unknown_command_answers_under_the_callers_id() {
        let mut input = Vec::new();
        write_frame(
            &mut input,
            br#"{"id":"sc-7","cmd":{"type":"command_from_a_newer_extension"}}"#,
        )
        .unwrap();
        let config = BridgeConfig {
            spawn_daemon: false,
            use_pipe: false,
            ..BridgeConfig::default()
        };
        let out = SharedBuf::default();
        serve_with(config, Cursor::new(input), out.clone()).unwrap();
        let got = frames(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "sc-7", "the caller's id must survive");
        assert!(matches!(got[0].reply, Reply::Error { .. }));
    }

    /// A reply wider than one native frame must still answer its id.
    #[test]
    fn oversized_reply_becomes_an_inband_error() {
        let resp = NativeResponse {
            id: "big".to_string(),
            reply: Reply::Error {
                message: "x".repeat(HOST_TO_BROWSER_MAX),
            },
        };
        let out = SharedBuf::default();
        send_reply(&Mutex::new(out.clone()), &resp).unwrap();
        let got = frames(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "big");
        match &got[0].reply {
            Reply::Error { message } => assert!(message.contains("too large"), "{message}"),
            other => panic!("expected an error reply, got {other:?}"),
        }
    }

    /// Daemon down (retry loop exhausted, spawn disabled) → an `error` reply,
    /// which the extension treats as "fall back to the browser download".
    /// Port-isolated: binds a socket, drops it, and aims there so a real
    /// daemon on the default port can't answer (the GUI keeps one alive).
    #[test]
    fn unreachable_daemon_becomes_error_reply() {
        // Take an ephemeral port, then vacate it: nothing listens there.
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let config = BridgeConfig {
            port,
            spawn_daemon: false,
            connect_attempts: 2,
            connect_delay: Duration::from_millis(20),
            use_pipe: false,
            rpc_token: Some("t".to_string()),
            ..BridgeConfig::default()
        };
        let seq = AtomicU64::new(0);
        let result = forward_once(&config, &seq, Command::List);
        assert!(result.is_err(), "expected unreachable daemon error");
    }
}
