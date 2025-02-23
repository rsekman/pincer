use std::os::unix::net::UnixStream;
use std::sync::Arc;

use clap::{Args, Parser as ArgParser, Subcommand};
use spdlog::prelude::*;
use tokio::{signal::ctrl_c, sync::Mutex, task::JoinSet};
use tokio_unix_ipc::channel_from_std;
use tokio_util::sync::CancellationToken;

use pincers::clipboard::Clipboard;
use pincers::daemon::{socket_path, Daemon, RegisterCommand, Request, Response};
use pincers::error::{Anyhow, Error};
use pincers::pincer::Pincer;
use pincers::register::{RegisterAddress, ADDRESS_HELP};

#[derive(ArgParser)]
#[command(name = "pincer")]
#[command(version = "0.1")]
struct CliOptions {
    #[arg(long = "log-level", help = "Log level", default_value = "Info")]
    log_level: spdlog::Level,
    #[command(subcommand)]
    command: CliCommands,
}

#[derive(Subcommand)]
enum CliCommands {
    /// Launch a pincer daemon
    Daemon {},

    /// Manipulate the daemon's register pointer
    Register(RegisterArgs),

    /// Yank from stdin into a register
    Yank {
        #[arg( help = ADDRESS_HELP)]
        address: Option<RegisterAddress>,
        #[arg(
            long = "mime-type",
            short = 't',
            help = "The MIME type of the content",
            default_value = "text/plain"
        )]
        mime: String,
    },
    /// Paste from a register to stdout
    Paste {
        #[arg(
            long = "mime-type",
            short = 't',
            help = "The MIME type to request",
            default_value = "text/plain"
        )]
        mime: String,
        #[arg( help = ADDRESS_HELP)]
        address: Option<RegisterAddress>,
    },

    ///Summarize contents of a register
    Show {
        #[arg(help = ADDRESS_HELP)]
        address: Option<RegisterAddress>,
    },

    /// List contents of all registers
    List {},
    // TODO: plain and json outputs
}

#[derive(Args)]
struct RegisterArgs {
    #[command(subcommand)]
    command: RegisterCommand,
}

async fn daemon() -> Result<(), Anyhow> {
    println!("Launching daemon");
    let p = Arc::new(Mutex::new(Pincer::new()));
    let token = CancellationToken::new();
    let d = Daemon::new(p.clone(), token.clone()).await?;

    // TODO use JoinSet here -- three tasks
    // - the clipboard listening
    //let wl = Clipboard::new(p.clone());
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { d.listen().await });
    let t = token.clone();
    tasks.spawn(async move {
        match ctrl_c().await {
            Err(e) => warn!("Could not catch Ctrl-C: {e}"),
            Ok(_) => {
                info!("Received SIGINT, exiting")
            }
        };
        t.cancel();
        Ok(())
    });
    tasks.join_all().await;

    Ok(())
}

async fn send_request(req: Request) -> Result<(), Anyhow> {
    let sp = socket_path();
    let (tx, rx) = UnixStream::connect(&sp)
        .and_then(channel_from_std::<Request, Response>)
        .map_err(|e| {
            error!(
                "Could not connect to daemon at {}: {e}",
                sp.to_string_lossy()
            );
            e
        })?;
    tx.send(req).await.map_err(|e| {
        error!("Could not transmit request: {e}");
        e
    })?;
    let req = rx.recv().await.map_err(|e| {
        error!("Could not send request to daemon: {e}");
        e
    })?;
    handle_response(req).map_err(Anyhow::msg)
}

fn handle_response(rsp: Response) -> Result<(), Error> {
    debug!("Received response: {rsp:?}");
    match rsp {
        Response::Yank(addr, resp) => handle_yank(addr, resp),
        _ => Ok(()),
    }
}

fn handle_yank(addr: RegisterAddress, resp: Result<usize, Error>) -> Result<(), Error> {
    resp.map(|n| info!("Yanked {n} bytes into {addr}"))
        .map_err(|e| {
            warn!("Could not yank into {addr}: {e}");
            e
        })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Anyhow> {
    let args = CliOptions::parse();
    use CliCommands::*;
    spdlog::default_logger().set_level_filter(spdlog::LevelFilter::MoreSevereEqual(args.log_level));

    let _ = match args.command {
        Daemon {} => daemon().await,
        Paste { mime, address } => send_request(Request::Paste(address, mime)).await,
        Show { address } => send_request(Request::Show(address)).await,
        List {} => send_request(Request::List()).await,
        Register(RegisterArgs { command }) => send_request(Request::Register(command)).await,
        Yank {
            address: _,
            mime: _,
        } => todo!(),
    };
    Ok(())
}
