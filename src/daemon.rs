use std::collections::BTreeMap;
use std::fs::{create_dir_all, File};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Subcommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use spdlog::prelude::*;
use tokio::{
    net::{UnixListener, UnixStream},
    select,
};
use tokio_unix_ipc::{channel_from_std, Sender};
use tokio_util::sync::CancellationToken;

use crate::error::{Anyhow, Error};
use crate::pincer::{Pincer, SeatPincerMap};
use crate::register::{MimeType, Register, RegisterAddress, RegisterSummary, ADDRESS_HELP};
use crate::seat::SeatSpecification;

const SOCKET_NAME: &str = "socket";
const LOCK_NAME: &str = "lock";

/// Struct for receiving commands over an IPC socket
#[derive(Debug)]
pub struct Daemon {
    pincers: Arc<Mutex<SeatPincerMap>>,
    lock: File,
}

impl Daemon {
    /// Create a new `Daemon`
    ///
    /// # Arguments
    ///
    /// * `pincers` - Reference to the pool of [Pincers](Pincer) to be shared between this Daemon instance
    /// and a [`Clipboard`](crate::clipboard::Clipboard) instance
    pub async fn new(pincers: Arc<Mutex<SeatPincerMap>>) -> Result<Daemon, Anyhow> {
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
        lock.try_lock_exclusive().map_err(|e| {
            error!("Could not acquire lock {}: {e}", lock_path_str);
            e
        })?;

        Ok(Daemon { pincers, lock })
    }

    /// Listen for commands from clients
    ///
    /// # Arguments
    ///
    /// * `token` - A [`CancellationToken`] that can be used to cancel listening
    pub async fn listen(&self, token: CancellationToken) -> Result<(), Anyhow> {
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
                _ = token.cancelled() => break Ok(()),
                conn  = socket.accept() => match conn  {
                    Ok((stream, _)) => {
                        let p = self.pincers.clone();
                        let t = token.clone();
                        tokio::spawn(async move { Self::accept(stream, p, t).await });
                    },
                    Err(e) => { error!("{e}"); break Err(Anyhow::new(e)) }
                }
            };
        }
    }

    async fn accept(
        conn: UnixStream,
        pincers: Arc<Mutex<SeatPincerMap>>,
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
        use std::io::ErrorKind::*;
        select! {
            _ = token.cancelled() => Ok(()),
            // tokio-ipc-unix sockets are connectionless, so we will only receive one message per
            // accept() call; there is no need to loop in this function
            msg = rx.recv() => {
                match msg {
                Ok(req) => Self::handle_request(req, &tx, pincers).await,
                Err(e) => match e.kind() {
                    TimedOut | ConnectionReset | ConnectionAborted => {
                        info!("Client disconnected: {e}");
                        Ok(())
                    }
                    // Malformed input is not a fatal error, the client could send valid data at a
                    // later point
                    InvalidData => {
                        warn!("Received invalid data from client: {e}");
                        Err(Anyhow::new(e))
                    },
                    // UnexpectedEof is likely propagated up from the Receiver because it tries
                    // to deserialize 0 bytes. Maybe upstream bug?
                    _ => {
                        warn!("Could not receive from client: {e}");
                        Err(Anyhow::new(e))
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
        pincers: Arc<Mutex<SeatPincerMap>>,
    ) -> Result<(), Anyhow> {
        debug!("Received request: {req:?}");

        // block to make sure the mutex is released before awaiting below
        let resp: Response = {
            use SeatSpecification::*;
            let mut pincers = pincers.lock().map_err(|e| {
                warn!("Could not acquire mutex: {e}");
                anyhow::Error::msg("Internal server error: {e}")
            })?;
            let pincer = match req.seat {
                Unspecified => pincers.values_mut().next(),
                Specified(ref k) => pincers.get_mut(k),
            };

            match pincer {
                Some(pincer) => Self::execute_request(req.request, pincer),
                None => {
                    let msg = format!(
                        "Received request for seat {:?}, but its clipboard is not managed by this daemon",
                        req.seat
                    );
                    warn!("{msg}");
                    Err(msg)
                }
            }
        };

        debug!("Sending response: {resp:?}");
        tx.send(resp).await.map_err(|e| {
            warn!("Sending response failed: {e}");
            Anyhow::new(e)
        })
    }

    // This method is factored out so the ? operator can be used
    fn execute_request(req: RequestType, pincer: &mut Pincer) -> Response {
        let res = match req {
            RequestType::Yank(addr, reg) => ResponseType::Yank(
                addr.unwrap_or_default(),
                pincer.yank_into(addr, reg.into_iter())?,
            ),
            RequestType::Paste(addr, mime) => ResponseType::Paste(
                addr.unwrap_or_default(),
                pincer.paste_from(addr, &mime)?.clone(),
            ),
            RequestType::Show(addr) => {
                ResponseType::Show(addr.unwrap_or_default(), pincer.register(addr).clone())
            }
            RequestType::List() => ResponseType::List(pincer.list()?),
            RequestType::Register(_) => todo!(),
        };
        Ok(res)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self
            .lock
            .unlock()
            .map_err(|e| warn!("Could not unlock {}: {e}.", lock_path().to_string_lossy()));
    }
}

fn get_directory() -> PathBuf {
    let mut dir = PathBuf::from(env!("XDG_DATA_HOME"));
    dir.push(env!("CARGO_PKG_NAME"));
    dir
}

/// Get the path to the socket on which a [`Daemon`] will be listening
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

/// Commands that manipulate the Daemon's register pointer
#[derive(Serialize, Deserialize, Subcommand, Debug)]
pub enum RegisterCommand {
    /// Command to set the Daemon's register pointer
    Select {
        #[arg(help = ADDRESS_HELP)]
        /// What the register pointer should be set to
        address: RegisterAddress,
    },
    /// Command to get the Daemon's register pointer
    Active {},
    /// Command to clear the Daemon's register pointer
    Clear {},
}

/// A request to the [`Daemon`]
#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    /// The seat to which the request applies
    pub seat: SeatSpecification,
    /// The type of the request, and its arguments, if any
    pub request: RequestType,
}

#[derive(Serialize, Deserialize, Debug)]
/// Possible requests to a [`Daemon`]. Each variant has a corresponding [`ResponseType`] variant for
/// the response from the [`Daemon`].
pub enum RequestType {
    /// Yank into the the specified register address, or into `"0` if None
    Yank(Option<RegisterAddress>, Register),
    /// Paste from the the specified register address, or from `"0` if None, requesting a specific
    /// MIME type
    Paste(Option<RegisterAddress>, MimeType),
    /// Show all the contents of the specified register address, or of `"0` if None
    Show(Option<RegisterAddress>),
    /// Like [`Show`](RequestType::Show), but for all registers
    List(),
    /// Manipulate the [`Daemon`]'s register pointer
    Register(RegisterCommand),
}

/// A response from the [`Daemon`]. An `Ok` variant if the request completed sucessfully, otherwise
/// an `Err` containing an error message.
pub type Response = Result<ResponseType, Error>;

#[derive(Serialize, Deserialize, Debug)]
/// Possible responses from the daemon. Each variant is the response to a corresponding
/// [`RequestType`] variant.
pub enum ResponseType {
    /// Contains the register address into which the data was yanked, and the total number of bytes
    /// yanked.
    Yank(RegisterAddress, usize),
    /// Contains the register address from the which data was pasted, and a buffer for the data
    Paste(RegisterAddress, Vec<u8>),
    /// Contains the register address which shown, and a map of MIME types to data buffers
    Show(RegisterAddress, Register),
    /// Contains a map of register addresses to summaries of their contents
    List(BTreeMap<RegisterAddress, RegisterSummary>),
    /// Contains the [`Daemon`]'s new register pointer
    Register(RegisterAddress),
}
