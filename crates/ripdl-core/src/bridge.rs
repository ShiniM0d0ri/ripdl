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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::rpc::{Command, Reply, DEFAULT_PORT};

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
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            connect_attempts: 20,
            connect_delay: Duration::from_millis(100),
            spawn_daemon: true,
            rpc_timeout: Duration::from_secs(30),
            use_pipe: true,
        }
    }
}

/// Serve the native-messaging loop on stdin/stdout until the pipe closes.
/// Per-request failures become `error` replies in-band; this function only
/// returns when stdin reaches EOF or the framing itself breaks.
pub fn serve(config: BridgeConfig) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut out = stdout.lock();
    let mut rpc_seq: u64 = 0;

    while let Some(frame) = read_frame(&mut input)? {
        let msg: NativeRequest = match serde_json::from_slice(&frame) {
            Ok(m) => m,
            Err(e) => {
                // Unparseable frames cannot be correlated to an id; the only
                // safe reply is a generic error under a synthetic id.
                let reply = NativeResponse {
                    id: "<parse>".to_string(),
                    reply: Reply::Error {
                        message: format!("bad native request: {e}"),
                    },
                };
                let json = serde_json::to_vec(&reply)?;
                write_frame(&mut out, &json)?;
                continue;
            }
        };
        let reply = match forward_once(&config, &mut rpc_seq, msg.cmd) {
            Ok(r) => r,
            Err(e) => Reply::Error {
                message: e.to_string(),
            },
        };
        let resp = NativeResponse { id: msg.id, reply };
        write_frame(&mut out, &serde_json::to_vec(&resp)?)?;
    }
    Ok(())
}

/// Push one command to the daemon over TCP, spawning it if absent (once per
/// call; every later request just races the freshly spawned listener).
fn forward_once(config: &BridgeConfig, rpc_seq: &mut u64, cmd: Command) -> Result<Reply> {
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
            .open(crate::transport::PIPE_NAME)
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

fn talk(config: &BridgeConfig, rpc_seq: &mut u64, cmd: &Command) -> Result<Reply> {
    *rpc_seq += 1;

    let mut stream = connect_daemon_sync(config.use_pipe, config.port)?;
    // Build the request after the connection exists: a failed connect (spawn
    // warming up) must not clone the payload per retry attempt.
    let req = crate::rpc::Request {
        id: *rpc_seq,
        cmd: cmd.clone(),
    };

    let mut line = serde_json::to_string(&req)?;
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

    /// Offline forward test: a one-shot fake daemon (std TcpListener on an
    /// ephemeral loopback port) answers one canned response; the bridge must
    /// regenerate its own RPC id, keep the caller's string id, and surface
    /// the daemon's reply.
    #[test]
    fn forwards_to_daemon_and_back() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut line = Vec::new();
            std::io::BufReader::new(&mut sock)
                .read_until(b'\n', &mut line)
                .unwrap();
            // Whatever id the bridge assigned, echo it back in a canned reply.
            let req: crate::rpc::Request = serde_json::from_slice(&line).unwrap();
            let resp = crate::rpc::Response {
                id: req.id,
                reply: Reply::Added {
                    id: "task-1".to_string(),
                },
            };
            let mut out = serde_json::to_string(&resp).unwrap();
            out.push('\n');
            use std::io::Write;
            (&mut sock).write_all(out.as_bytes()).unwrap();
            req
        });

        let config = BridgeConfig {
            port,
            spawn_daemon: false,
            connect_attempts: 1,
            use_pipe: false,
            ..BridgeConfig::default()
        };
        let mut seq = 0;
        let reply = forward_once(
            &config,
            &mut seq,
            Command::Add {
                url: "https://x/f.zip".to_string(),
                dest_dir: None,
                headers: Default::default(),
                start_at: None,
                kind: Default::default(),
                sequential: Default::default(),
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            serde_json::json!({"type": "added", "id": "task-1"})
        );
        let req = server.join().unwrap();
        assert_eq!(req.id, 1, "bridge assigns its own numeric rpc ids");
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
            ..BridgeConfig::default()
        };
        let mut seq = 0;
        let result = forward_once(&config, &mut seq, Command::List);
        assert!(result.is_err(), "expected unreachable daemon error");
    }
}
