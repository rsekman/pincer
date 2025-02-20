use std::collections::BTreeMap;
use std::fs::{create_dir_all, File};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Subcommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::{
    net::{UnixListener, UnixStream},
    select,
    sync::Mutex,
};
use tokio_unix_ipc::{channel_from_std, Sender};
use tokio_util::sync::CancellationToken;

use crate::error::ErrorT;
use crate::pincer::Pincer;
use crate::register::{Register, RegisterAddress, RegisterSummary, ADDRESS_HELP};

const SOCKET_NAME: &str = "socket";
const LOCK_NAME: &str = "lock";

/// Struct for receiving commands over a socket
// The Daemon struct and the Clipboard struct both contain Arc<Mutex<_>> of the manager state. This
// is a minimal form of the message passing pattern for sharing state; each acquires the lock only
// when they need to access the state. listen() cannot be methods on the state directly; that would
// cause deadlocks as both need to acquire the lock.
pub struct Daemon {
    pincer: Arc<Mutex<Pincer>>,
    token: CancellationToken,
    lock: File,
}

impl Daemon {
    /// Create a new Daemon managing the state in the argument
    pub async fn new(
        pincer: Arc<Mutex<Pincer>>,
        token: CancellationToken,
    ) -> Result<Daemon, ErrorT> {
        let dir = get_directory();
        create_dir_all(&dir)
            .or_else(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => Ok(()),
                _ => Err(e),
            })
            .map_err(|e| {
                format!(
                    "FATAL ERROR: Could not create directory {}: {e}",
                    dir.to_string_lossy()
                )
            })?;

        // Try to acquire lock
        let lock_path = lock_path();
        let lock_path_str = lock_path.to_string_lossy();
        let lock = File::create(&lock_path).map_err(|e| {
            format!(
                "FATAL ERROR: Could not open lock file {}: {e}",
                lock_path_str
            )
        });
        let _ = lock
            .as_ref()
            .map(FileExt::try_lock_exclusive)
            .map_err(|e| format!("FATAL ERROR: Could not acquire lock {}: {e}", lock_path_str))?;

        Ok(Daemon {
            pincer,
            token,
            lock: lock?,
        })
    }

    /// Listen for commands
    pub async fn listen(&mut self) -> Result<(), ErrorT> {
        let sock_path = socket_path();
        let _ = std::fs::remove_file(&sock_path);
        let socket = UnixListener::bind(&sock_path).map_err(|e| {
            format!(
                "FATAL ERROR: Could not bind to socket {}: {e}",
                sock_path.to_string_lossy()
            )
        })?;

        loop {
            select! {
                _ = self.token.cancelled() => break Ok(()),
                conn  = socket.accept() => match conn  {
                    Ok((stream, _)) => {
                        let p = self.pincer.clone();
                        tokio::spawn(async move { Self::accept(stream, p).await });
                    },
                    Err(e) => { break Err(format!("FATAL ERROR: {e}")); }
                }
            };
        }
    }

    async fn accept(conn: UnixStream, pincer: Arc<Mutex<Pincer>>) -> Result<(), ErrorT> {
        let conn = dbg!(conn)
            .into_std()
            .map_err(|e| format!("FATAL ERROR: {e}"))?;
        let (tx, rx) = channel_from_std::<Response, Request>(conn)
            .map_err(|_| "FATAL ERROR: could not accept incoming connection: {e}")?;
        loop {
            match rx.recv().await {
                Ok(req) => Self::handle_request(req, &tx, &pincer).await?,
                Err(e) => break Err(format!("WARNING: error receiving data: {e}")),
            }
        }
    }

    /// Handle arriving commands
    async fn handle_request(
        req: Request,
        tx: &Sender<Response>,
        pincer: &Arc<Mutex<Pincer>>,
    ) -> Result<(), ErrorT> {
        let mut pincer = pincer.lock().await;
        let resp = match dbg!(req) {
            Request::Yank(addr, reg) => Response::Yank(pincer.yank(addr, reg.into_iter())),
            Request::Paste(addr, mime) => Response::Paste(pincer.paste(addr, &mime).cloned()),
            Request::Show(addr) => Response::Show(Ok(pincer.register(addr).clone())),
            Request::List() => Response::List(pincer.list()),
            Request::Register(_) => todo!(),
        };
        tx.send(dbg!(resp))
            .await
            .map_err(|e| format!("WARNING: sending response failed: {e}"))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.lock.unlock().map_err(|e| {
            eprintln!(
                "WARNING: Could unlock {}: {e}.",
                lock_path().to_string_lossy()
            )
        });
    }
}

fn get_directory() -> PathBuf {
    let mut dir = PathBuf::from(env!("XDG_DATA_HOME"));
    dir.push(env!("CARGO_PKG_NAME"));
    dir
}

pub fn socket_path() -> PathBuf {
    let mut dir = get_directory();
    dir.push(SOCKET_NAME);
    dir
}

fn lock_path() -> PathBuf {
    let mut dir = get_directory();
    dir.push(LOCK_NAME);
    dir
}

#[derive(Serialize, Deserialize, Subcommand, Debug)]
pub enum RegisterCommand {
    Select {
        #[arg(help = ADDRESS_HELP)]
        address: RegisterAddress,
    },
    Active {},
    Clear {},
}

#[derive(Serialize, Deserialize, Debug)]
/// Possible requests to the daemon
pub enum Request {
    Yank(Option<RegisterAddress>, Register),
    Paste(Option<RegisterAddress>, String),
    Show(Option<RegisterAddress>),
    List(),
    Register(RegisterCommand),
}

#[derive(Serialize, Deserialize, Debug)]
/// Possible responses from the daemon
pub enum Response {
    Yank(Result<(RegisterAddress, usize), ErrorT>),
    Paste(Result<Vec<u8>, ErrorT>),
    Show(Result<Register, ErrorT>),
    List(Result<BTreeMap<RegisterAddress, RegisterSummary>, ErrorT>),
    Register(Result<(), ErrorT>),
}
