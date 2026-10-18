//! Transport addressing shared by the daemon (listeners) and clients (CLI,
//! native-messaging bridge). One place so frontends never hardcode endpoints.

use std::io;

use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

/// Windows named-pipe endpoint. A fixed name: no port conflicts, no
/// listeners visible outside the local session.
pub const PIPE_NAME: &str = "\\\\.\\pipe\\ripdl-rpc";

/// ripdl's per-user data directory (queue, config, manifests).
pub fn data_dir() -> io::Result<std::path::PathBuf> {
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

/// Connect to the running daemon, preferring the local-session transports
/// (named pipe on Windows / unix socket elsewhere) and falling back to TCP
/// loopback for mixed-version dev environments.
pub async fn connect_daemon() -> io::Result<AsyncStream> {
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
            match ClientOptions::new().open(PIPE_NAME) {
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
    let port = crate::rpc::DEFAULT_PORT;
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
