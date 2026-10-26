//! Transport addressing shared by the daemon (listeners) and clients (CLI,
//! native-messaging bridge). One place so frontends never hardcode endpoints.

use std::io;

use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

/// Windows named-pipe endpoint. A fixed name: no port conflicts, no
/// listeners visible outside the local session.
pub const PIPE_NAME: &str = "\\\\.\\pipe\\ripdl-rpc";

/// Test seams: an isolated daemon (the e2e runner starts one beside the
/// user's own) points all three at private values. Unset in normal use.
pub const ENV_DATA_DIR: &str = "RIPDL_DATA_DIR";
pub const ENV_PIPE: &str = "RIPDL_RPC_PIPE";
pub const ENV_PORT: &str = "RIPDL_RPC_PORT";

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Named-pipe endpoint in use: [`PIPE_NAME`] unless `RIPDL_RPC_PIPE` is set.
pub fn pipe_name() -> String {
    env_value(ENV_PIPE).unwrap_or_else(|| PIPE_NAME.to_string())
}

/// TCP loopback port in use: the default unless `RIPDL_RPC_PORT` is set.
pub fn rpc_port() -> u16 {
    env_value(ENV_PORT)
        .and_then(|v| v.parse().ok())
        .unwrap_or(crate::rpc::DEFAULT_PORT)
}

/// ripdl's shared per-user data directory for queue, settings, and managed
/// extension files. Kept aligned with `dirs::data_local_dir()` on all OSes.
pub fn app_data_dir() -> io::Result<std::path::PathBuf> {
    if let Some(dir) = env_value(ENV_DATA_DIR) {
        return Ok(dir.into());
    }
    dirs::data_local_dir()
        .map(|dir| dir.join("ripdl"))
        .ok_or_else(|| io::Error::other("cannot resolve local data directory"))
}

/// Base directory for the local RPC socket on Unix and native-host files.
/// Queue, settings, and the managed extension use [`app_data_dir`].
pub fn data_dir() -> io::Result<std::path::PathBuf> {
    if let Some(dir) = env_value(ENV_DATA_DIR) {
        return Ok(dir.into());
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| std::path::PathBuf::from(d).join("ripdl"))
            .ok_or_else(|| io::Error::other("%LOCALAPPDATA% not set"))
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(std::path::PathBuf::from(xdg).join("ripdl"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::other("neither XDG_CONFIG_HOME nor HOME set"))?;
        Ok(std::path::PathBuf::from(home).join(".config").join("ripdl"))
    }
}

#[cfg(not(windows))]
/// Unix domain socket endpoint (`<data_dir>/rpc.sock`).
pub fn unix_socket_path() -> io::Result<std::path::PathBuf> {
    Ok(data_dir()?.join("rpc.sock"))
}

/// A connected daemon side, boxed so callers needn't know which transport
/// answered. The Unpin supertrait keeps tokio's forwarding impls happy.
pub type AsyncStream = Box<dyn AsyncReadWrite + Send + Unpin>;

pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized> AsyncReadWrite for T {}

/// Connect to the running daemon and authenticate: the first line on every
/// connection is the token from [`rpc_token_path`]. A daemon that is not
/// running (or has not written its token yet) is an error here, not a hang.
pub async fn connect_daemon() -> io::Result<AsyncStream> {
    let mut stream = connect_endpoint().await?;
    let token = read_rpc_token().map_err(|e| {
        io::Error::new(e.kind(), format!("cannot read the daemon token ({e}); is the daemon running?"))
    })?;
    stream.write_all(auth_line(&token).as_bytes()).await?;
    Ok(stream)
}

/// Open the daemon's transport without authenticating. Prefers the
/// local-session transports (named pipe on Windows / unix socket elsewhere)
/// and falls back to TCP loopback for mixed-version dev environments.
/// Clients use [`connect_daemon`], which adds the auth line.
pub async fn connect_endpoint() -> io::Result<AsyncStream> {
    let mut other_err: Option<io::Error> = None;
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        // The server keeps one free instance; while it recycles there can be
        // a short busy window - retry that only (ERROR_PIPE_BUSY). Retrying
        // every error made an absent daemon burn 5x80ms per call before the
        // TCP fallback answered.
        const PIPE_BUSY: i32 = 231; // ERROR_PIPE_BUSY
        for attempt in 0..6 {
            match ClientOptions::new().open(pipe_name()) {
                Ok(client) => return Ok(Box::new(client)),
                Err(e) if attempt < 5 && e.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                }
                Err(e) => {
                    other_err = Some(e);
                    break;
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(path) = unix_socket_path() {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(sock) => return Ok(Box::new(sock)),
                Err(e) => other_err = Some(e),
            }
        }
    }
    // Surface why the local transport failed too: a bare TCP error would
    // hide the actual cause (e.g. access denied on the pipe).
    let port = rpc_port();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
    match tcp {
        Ok(s) => Ok(Box::new(s) as AsyncStream),
        Err(e) => match other_err {
            Some(local) => Err(io::Error::other(format!(
                "no daemon endpoint reached: local transport: {local}; tcp 127.0.0.1:{port}: {e}"
            ))),
            None => Err(e),
        },
    }
}

/// `<data_dir>/rpc.token`: the per-start secret every RPC connection must
/// present first. Readable only by the user who owns the data directory.
pub fn rpc_token_path() -> io::Result<std::path::PathBuf> {
    Ok(data_dir()?.join("rpc.token"))
}

/// A fresh token: 244 bits from two random v4 UUIDs, as 64 hex digits.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Persist the token. The daemon calls this only after it owns its TCP
/// endpoint: a second daemon that fails to start must never replace the
/// token of the one that is running.
pub fn write_rpc_token(token: &str) -> io::Result<()> {
    let path = rpc_token_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("token.tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// The token the running daemon (started from this data directory) expects.
pub fn read_rpc_token() -> io::Result<String> {
    let raw = std::fs::read_to_string(rpc_token_path()?)?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(io::Error::other("rpc token file is empty"));
    }
    Ok(token)
}

/// Remove the token file when the daemon stops, but only while it still
/// holds `token`: a newer daemon may already have written its own.
pub fn clear_rpc_token(token: &str) {
    if read_rpc_token().is_ok_and(|current| current == token) {
        if let Ok(path) = rpc_token_path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The first line of every connection: `{"auth":"<token>"}`. It carries no
/// request id, so it can never be mistaken for a command.
pub fn auth_line(token: &str) -> String {
    format!("{}\n", serde_json::json!({ "auth": token }))
}

/// Whether `line` is the auth line for `token`.
pub fn auth_matches(line: &str, token: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    match value.get("auth").and_then(|v| v.as_str()) {
        Some(given) => constant_time_eq(given.as_bytes(), token.as_bytes()),
        None => false,
    }
}

/// Compare without an early exit, so reply timing reveals no prefix.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_line_matches_only_its_token() {
        let line = auth_line("abc123");
        assert!(line.ends_with('\n'));
        assert!(auth_matches(line.trim_end(), "abc123"));
        assert!(!auth_matches(line.trim_end(), "abc124"));
        assert!(!auth_matches(line.trim_end(), "abc12"));
        assert!(!auth_matches("{\"id\":1,\"cmd\":{\"type\":\"list\"}}", "abc123"));
        assert!(!auth_matches("not json", "abc123"));
    }

    #[test]
    fn generated_tokens_are_long_and_distinct() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
