//! One GUI per machine session.
//!
//! The first process owns a local endpoint (Windows named pipe
//! `\\.\pipe\ripdl-gui`, unix socket `<data_dir>/gui.sock`). A second launch
//! connects, sends `show`, and exits; the running instance brings its window
//! back (same behavior as clicking the tray icon). Kernel objects disappear
//! with the process, so there is no stale state to clean up - except a unix
//! socket file after a crash, which is detected and removed.
//!
//! Scope: per machine (not per desktop session). On a multi-user box the
//! signal is a harmless "show window" request; the guard still prevents two
//! GUIs from sharing one daemon/queue view.

use std::io;

#[cfg(windows)]
const GUARD_ENDPOINT: &str = "\\\\.\\pipe\\ripdl-gui";
/// `CreateNamedPipeW` reports this when another process owns the first
/// instance (`first_pipe_instance(true)`), i.e. an instance already exists.
#[cfg(windows)]
const ERROR_ACCESS_DENIED: i32 = 5;
#[cfg(windows)]
const ERROR_FILE_NOT_FOUND: i32 = 2;
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

/// Owning handle for the process that won the race.
pub struct Guard {
    /// Tokio's pipe registration is bound to this runtime's I/O driver, so
    /// the guard carries the runtime for its whole lifetime.
    #[cfg(windows)]
    rt: tokio::runtime::Runtime,
    #[cfg(windows)]
    pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    #[cfg(windows)]
    name: String,
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
}

/// Claim the GUI slot.
///
/// `Ok(Some(guard))` - this process is the only instance and must start.
/// `Ok(None)` - another instance was running and has been asked to show.
/// `Err(_)` - the guard could not be created at all; the caller should start
/// anyway (failing open beats refusing to launch the app).
pub fn claim() -> io::Result<Option<Guard>> {
    #[cfg(windows)]
    {
        claim_named(GUARD_ENDPOINT)
    }
    #[cfg(unix)]
    {
        let path = ripdl_core::transport::data_dir()?.join("gui.sock");
        claim_path(&path)
    }
}

impl Guard {
    /// Start the accept loop; `on_show` runs on a background thread each time
    /// a later launch asks the window to come forward.
    pub fn spawn_listener(self, on_show: impl Fn() + Send + 'static) {
        #[cfg(windows)]
        {
            let Guard { rt, mut pipe, name } = self;
            std::thread::spawn(move || {
                use tokio::io::AsyncReadExt;
                // Pipe creation registers with the runtime's I/O driver.
                let _entered = rt.enter();
                loop {
                    if rt.block_on(pipe.connect()).is_err() {
                        // Instance churn; recycle and keep serving.
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        match tokio::net::windows::named_pipe::ServerOptions::new().create(&name) {
                            Ok(next) => pipe = next,
                            Err(_) => return,
                        }
                        continue;
                    }
                    let mut buf = [0u8; 32];
                    let n = rt.block_on(pipe.read(&mut buf)).unwrap_or(0);
                    if String::from_utf8_lossy(&buf[..n]).contains("show") {
                        on_show();
                    }
                    // Reserve the next instance before dropping this one so
                    // the window where no pipe exists stays tiny.
                    match tokio::net::windows::named_pipe::ServerOptions::new().create(&name) {
                        Ok(next) => pipe = next,
                        Err(_) => return,
                    }
                }
            });
        }
        #[cfg(unix)]
        {
            std::thread::spawn(move || {
                use std::io::Read;
                for conn in self.listener.incoming() {
                    let Ok(mut conn) = conn else { continue };
                    let mut buf = [0u8; 32];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    if String::from_utf8_lossy(&buf[..n]).contains("show") {
                        on_show();
                    }
                }
            });
        }
    }
}

#[cfg(windows)]
fn claim_named(name: &str) -> io::Result<Option<Guard>> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let created = {
        let _entered = rt.enter();
        ServerOptions::new().first_pipe_instance(true).create(name)
    };
    match created {
        Ok(pipe) => Ok(Some(Guard {
            rt,
            pipe,
            name: name.to_owned(),
        })),
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
            drop(rt);
            // Somebody owns the pipe, so this launch must not open a second
            // window. The "show" hint is best effort: a normal-integrity
            // launch cannot poke an elevated instance (UIPI blocks the pipe
            // write), and starting anyway would put two windows on one
            // daemon - the tray icon is still there to bring the first one
            // back.
            let _ = notify_named(name);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Ask the running instance to show itself. Short retries cover the tiny
/// window between the listener recycling pipe instances.
#[cfg(windows)]
pub fn notify_named(name: &str) -> io::Result<()> {
    use std::io::Write;
    for attempt in 0..10 {
        match std::fs::OpenOptions::new().write(true).open(name) {
            Ok(mut pipe) => {
                pipe.write_all(b"show\n")?;
                return Ok(());
            }
            Err(e)
                if attempt < 9
                    && matches!(
                        e.raw_os_error(),
                        Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PIPE_BUSY)
                    ) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no GUI instance answered"))
}

#[cfg(unix)]
fn claim_path(path: &std::path::Path) -> io::Result<Option<Guard>> {
    use std::os::unix::net::{UnixListener, UnixStream};

    match UnixListener::bind(path) {
        Ok(listener) => Ok(Some(Guard { listener })),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => match UnixStream::connect(path) {
            Ok(mut conn) => {
                use std::io::Write;
                conn.write_all(b"show\n")?;
                Ok(None)
            }
            // Stale socket from a crashed instance: take it over.
            Err(_) => {
                let _ = std::fs::remove_file(path);
                UnixListener::bind(path).map(|listener| Some(Guard { listener }))
            }
        },
        Err(e) => Err(e),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn second_claim_notifies_then_reports_existing() {
        let name = format!("\\\\.\\pipe\\ripdl-gui-test-{}", std::process::id());
        let guard = claim_named(&name).expect("first claim").expect("owner");
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        guard.spawn_listener(move || {
            h.fetch_add(1, Ordering::SeqCst);
        });

        // The accept loop starts on a thread; notify's retries cover it.
        notify_named(&name).expect("notify");
        let mut waited = 0;
        while hits.load(Ordering::SeqCst) == 0 && waited < 2000 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "listener saw the show request"
        );

        // A second claim must hand off instead of becoming the owner.
        assert!(
            claim_named(&name).expect("second claim").is_none(),
            "second instance handed off instead of starting"
        );
    }
}
