use std::collections::BTreeMap;
use std::fs::{create_dir_all, File};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Subcommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use spdlog::prelude::*;
use tokio::{
    net::{UnixListener, UnixStream},
    select,
    sync::Mutex,
};
use tokio_unix_ipc::{channel_from_std, Sender};
use tokio_util::sync::CancellationToken;

use crate::error::{Anyhow, Error};
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
    ) -> Result<Daemon, Anyhow> {
        let dir = get_directory();
        create_dir_all(&dir)
            .or_else(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => Ok(()),
                _ => Err(e),
            })
            .map_err(|e| {
                error!("Could not create directory {}: {e}", dir.to_string_lossy());
                e
            })?;

        // Try to acquire lock
        let lock_path = lock_path();
        let lock_path_str = lock_path.to_string_lossy();
        let lock = File::create(&lock_path).map_err(|e| {
            error!("Could not open lock file {}: {e}", lock_path_str);
            e
        })?;
        let _ = lock.try_lock_exclusive().map_err(|e| {
            error!("Could not acquire lock {}: {e}", lock_path_str);
            e
        })?;

        Ok(Daemon {
            pincer,
            token,
            lock,
        })
    }

    /// Listen for commands
    pub async fn listen(&mut self) -> Result<(), Anyhow> {
        let sock_path = socket_path();
        let _ = std::fs::remove_file(&sock_path);
        let socket = UnixListener::bind(&sock_path).map_err(|e| {
            error!(
                "Could not bind to socket {}: {e}",
                sock_path.to_string_lossy()
            );
            e
        })?;

        loop {
            select! {
                _ = self.token.cancelled() => break Ok(()),
                conn  = socket.accept() => match conn  {
                    Ok((stream, _)) => {
                        let p = self.pincer.clone();
                        let t = self.token.clone();
                        tokio::spawn(async move { Self::accept(stream, p, t).await });
                    },
                    Err(e) => { error!("{e}"); break Err(Anyhow::new(e)) }
                }
            };
        }
    }

    async fn accept(
        conn: UnixStream,
        pincer: Arc<Mutex<Pincer>>,
        token: CancellationToken,
    ) -> Result<(), Anyhow> {
        // No errors from handling a request should be fatal, but the short-circuit ? operator is
        // more convenient than if let Some(...)
        let (tx, rx) = conn
            .into_std()
            .and_then(channel_from_std::<Response, Request>)
            .map_err(|e| {
                error!("Could not accept incoming connection: {e}");
                e
            })?;
        loop {
            use std::io::ErrorKind::*;
            select! {
                    _ = token.cancelled() => return Ok(()),
                    msg = rx.recv() =>
                match msg {
                    Ok(req) => Self::handle_request(req, &tx, &pincer).await?,
                    Err(e) => match e.kind() {
                        TimedOut | ConnectionReset | ConnectionAborted => {
                            // We're done with this connection
                            info!("{e}");
                            return Ok(());
                        }
                        // Malformed input is not a fatal error, the client could send valid data at a
                        // later point
                        InvalidData => warn!("Received invalid data from client: {e}"),
                        _ => {
                            warn!("Could not receive from client: {e}")
                        }
                    },
                }
            }
        }
    }

    /// Handle arriving commands
    async fn handle_request(
        req: Request,
        tx: &Sender<Response>,
        pincer: &Arc<Mutex<Pincer>>,
    ) -> Result<(), Anyhow> {
        let mut pincer = pincer.lock().await;
        let resp = match dbg!(req) {
            Request::Yank(addr, reg) => {
                Response::Yank(addr.unwrap_or_default(), pincer.yank(addr, reg.into_iter()))
            }
            Request::Paste(addr, mime) => {
                Response::Paste(addr.unwrap_or_default(), pincer.paste(addr, &mime).cloned())
            }
            Request::Show(addr) => {
                Response::Show((addr.unwrap_or_default(), Ok(pincer.register(addr).clone())))
            }
            Request::List() => Response::List(pincer.list()),
            Request::Register(_) => todo!(),
        };
        tx.send(resp).await.map_err(|e| {
            warn!("Sending response failed: {e}");
            Anyhow::new(e)
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self
            .lock
            .unlock()
            .map_err(|e| warn!("Could unlock {}: {e}.", lock_path().to_string_lossy()));
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
    Yank(RegisterAddress, Result<usize, Error>),
    Paste(RegisterAddress, Result<Vec<u8>, Error>),
    Show((RegisterAddress, Result<Register, Error>)),
    List(Result<BTreeMap<RegisterAddress, RegisterSummary>, Error>),
    Register(Result<(), Error>),
}
