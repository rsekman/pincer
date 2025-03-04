#![allow(unused_imports, unused_variables, dead_code)]
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::fs::File;
use std::io::{Error as IOError, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

use os_pipe::{pipe, PipeReader};

use spdlog::prelude::*;

use tokio::io::unix::AsyncFd;
use tokio::select;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use wayland_client::{
    backend::WaylandError,
    event_created_child,
    globals::{registry_queue_init, BindError, GlobalError, GlobalListContents},
    protocol::{
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
    },
    ConnectError, Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::{self, ZwlrDataControlManagerV1},
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::error::{log_and_pass_on, Anyhow, Error};
use crate::pincer::{Pincer, SeatPincerMap};
use crate::register::MimeType;
use crate::seat::{SeatData, SeatIdentifier};

/// Struct that interfaces with the Wayland compositor
#[derive(Debug)]
pub struct Clipboard {
    queue: Arc<Mutex<EventQueue<Clipboard>>>,
    pincers: Arc<Mutex<SeatPincerMap>>,
    conn: Connection,
    manager: ZwlrDataControlManagerV1,
    seats: HashMap<WlSeat, SeatData>,
    offers: HashMap<ZwlrDataControlOfferV1, HashMap<MimeType, PipeReader>>,
}

impl Clipboard {
    /// Create a new `Clipboard` instance that is connected to the Wayland compositor.
    ///
    /// # Arguments
    ///
    /// * `pincers` - Reference to the pool of Pincers to be shared between this `Clipboard` and a
    ///   [`Daemon`](crate::daemon::Daemon) instance
    pub fn new(pincers: Arc<Mutex<SeatPincerMap>>) -> Result<Self, Anyhow> {
        // Connect to the Wayland compositor
        let conn = Connection::connect_to_env()
            .map_err(log_and_pass_on!("Could not connect to to Wayland"))?;

        // Initialize an event queue for the registry
        let (globals, queue) = registry_queue_init(&conn).map_err(log_and_pass_on!(
            "Could not initialize Wayland registry queue"
        ))?;

        // Verify that the compositor supports the protocol
        let qh = &queue.handle();
        let data_control_version = 1;
        let manager = globals
            // This type annotation is superfluous, but illustrates how to specify the type of
            // global to bind.
            .bind::<ZwlrDataControlManagerV1, _, _>(
                qh,
                data_control_version..=data_control_version,
                (),
            )
            .map_err(log_and_pass_on!(format!(
                "Compositor does not support {} v{data_control_version}",
                ZwlrDataControlManagerV1::interface().name
            )))?;

        // Find out which seats exist
        let registry = globals.registry();
        let seats = globals.contents().with_list(|globals| {
            globals
                .iter()
                .filter(|global| {
                    global.interface == WlSeat::interface().name && global.version >= 2
                })
                .map(|global| {
                    let seat = registry.bind(global.name, 2, qh, ());
                    let sd = SeatData {
                        numeric_name: global.name,
                        ..Default::default()
                    };
                    (seat, sd)
                })
                .collect()
        });

        let queue = Arc::new(Mutex::new(queue));
        let mut clip = Clipboard {
            queue,
            pincers,
            conn,
            manager,
            seats,
            offers: HashMap::new(),
        };
        clip.grab()?;

        Ok(clip)
    }

    /// Start listening to events from the Wayland compositor
    ///
    /// # Arguments
    ///
    /// * `token` - A [`CancellationToken`] that can be used to cancel listening
    pub async fn listen(&mut self, token: CancellationToken) -> Result<(), Anyhow> {
        loop {
            // cloning prevents referencing self
            let q = self.queue.clone();
            let read_guard = {
                let mut q = q.lock().unwrap();
                let _ = q
                    .flush()
                    .map_err(|e| warn!("Wayland communication error: {e}"));
                let _ = q
                    .dispatch_pending(self)
                    .map_err(|e| warn!("Wayland communication error: {e}"));
                q.prepare_read().unwrap()
                // lock is released here
            };
            let poll_wl = async move {
                {
                    let conn = read_guard.connection_fd();
                    let fd = AsyncFd::new(conn.as_fd()).unwrap();
                    let _ = fd.readable().await;
                }
                read_guard.read()
            };
            select! {
                _ = token.cancelled() => break,
                read = poll_wl => {
                    let mut q = q.lock().unwrap();
                    match read {
                        Ok(n) => {
                            let _ = q
                                .dispatch_pending(self)
                                .map_err(|e| warn!("Wayland communication error: {e}"));
                        }
                        Err(e) => {
                            warn!("Wayland communication error: {e}");
                        }
                    };
                }
            }
        }

        Ok(())
    }

    fn send<T>(&self, seat: &WlSeat, mime: MimeType, fd: T)
    where
        T: Into<File>,
    {
        let Some(seat_data) = self.seats.get(seat) else {
            warn!("Received request from unknown seat {seat:?}");
            return;
        };
        let Some(ref seat_name) = seat_data.name else {
            warn!("Received request from unknown seat {seat:?}");
            return;
        };
        let pincers = self.pincers.lock().unwrap();
        let Some(pincer) = pincers.get(seat_name) else {
            warn!("Clipboard of seat {seat_data:?} is not managed");
            return;
        };
        let reg = pincer.get_active();
        trace!("Received request for {mime}, using {reg}",);
        match pincer.paste(&mime) {
            Ok(data) => {
                let mut target_file: File = fd.into();
                match target_file.write(data) {
                    Ok(n) => {
                        trace!("Sent {n} bytes of type {mime} from {reg}",)
                    }
                    Err(e) => warn!("Could not send data: {e}"),
                }
            }
            Err(e) => {
                trace!("{e}")
            }
        }
    }

    fn grab(&mut self) -> Result<(), Anyhow> {
        // request each seat's data device
        let qh = self.queue.lock().unwrap().handle();
        for (seat, data) in &mut self.seats {
            let device = self.manager.get_data_device(seat, &qh, seat.clone());
            data.set_device(Some(device));
        }

        // After this round-trip we will know about all MIME types and have sent requests for them
        // ...
        self.roundtrip()?;
        // ... and after this round-trip the compositor will have written the data to the pipes
        self.roundtrip()?;

        // request all the data!
        let xs: Vec<_> = self
            .seats
            .iter()
            .filter_map(|(_, data)| Option::zip(data.name.clone(), data.offer.clone()))
            // have to collect rather than use the iterator directly because the iterator will have
            // an immutable borrow of self
            .collect();
        for (name, offer) in xs {
            self.yank_from_selection(&name, &offer)?;
        }

        // round-trip
        // grab the clipboard
        Ok(())
    }

    fn yank_from_selection(
        &mut self,
        seat: &SeatIdentifier,
        offer: &ZwlrDataControlOfferV1,
    ) -> Result<(), Anyhow> {
        let mimes = self
            .offers
            .get(offer)
            .ok_or(anyhow::Error::msg("Offer {offer:?} not registered"))?;
        let mut pincers = self.pincers.lock().unwrap();
        let pincer = pincers.get_mut(seat).ok_or_else(|| {
            error!("No pincer");
            anyhow::Error::msg("No pincer found for {seat:?}")
        })?;

        let read_data = |(mime, mut f): (&String, &PipeReader)| {
            let mut data = Vec::new();
            match f.read_to_end(&mut data) {
                Ok(n) => {
                    debug!("Read {n} bytes of MIME {mime}");
                    Some((mime.clone(), data))
                }
                Err(e) => {
                    error!("While trying to yank data of MIME {mime}, could not read data from {f:?}: {e}");
                    None
                }
            }
        };
        pincer
            .yank(mimes.iter().filter_map(read_data))
            .map(|_| {})
            .map_err(anyhow::Error::msg)
    }

    fn roundtrip(&mut self) -> Result<(), Anyhow> {
        let q = self.queue.clone();
        let res = q.lock().unwrap().roundtrip(self);
        res.map(|_| {}).map_err(|e| {
            error!("Wayland communication error: {e}");
            anyhow::Error::new(e)
        })
    }
}

/// Events are dispatched on the [`WlRegistry`] when global objects appear or disappear
impl Dispatch<WlRegistry, GlobalListContents> for Clipboard {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        use wl_registry::Event::*;
        match event {
            // A new global appeared, it could be a Seat. If so, we should prepare to manage its
            // clipboard.
            Global {
                name,
                interface,
                version,
            } => {
                if interface == WlSeat::interface().name && version >= 2 {
                    info!("Seat #{name} appeared, preparing to manage its clipboard");
                    let seat = registry.bind(name, 2, qhandle, ());
                    let sd = SeatData {
                        numeric_name: name,
                        ..Default::default()
                    };
                    state.seats.insert(seat, sd);
                }
            }
            // A global disappeared, it could be a Seat. If so, we should un-manage it.
            GlobalRemove { name } => {
                if let Some((_, seat_data)) =
                    state.seats.iter().find(|(_, sd)| sd.numeric_name == name)
                {
                    info!("Seat {seat_data:?} disappeared, un-managing its clipboard");
                    let mut pincers = state.pincers.lock().unwrap();
                    if seat_data
                        .name
                        .as_ref()
                        .and_then(|n| pincers.remove(n))
                        .is_none()
                    {
                        warn!("Tried to unmanage clipboard of seat {seat_data:?}, but it was not managed");
                    }
                }
                state.seats.retain(|_, sd| sd.numeric_name != name);
            }
            _ => {}
        }
    }
}

/// Events are dispatched on a [`WlSeat`] to notify us of its (string) name and its capabilities (input
/// devices) We only care about the name. A [`Pincer`] instance is created for each seat, accessed by
/// name.
impl Dispatch<WlSeat, ()> for Clipboard {
    fn event(
        state: &mut Clipboard,
        seat: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Clipboard>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            let data = state.seats.get_mut(seat).unwrap();
            data.set_name(name.clone());
            debug!("Registered seat {data:?}");
            let mut pincers = state.pincers.lock().unwrap();
            if let Entry::Vacant(e) = pincers.entry(name) {
                e.insert(Pincer::new());
                info!("Managing clipboard for {data:?}");
            } else {
                warn!("Clipboard of seat {data:?} already managed");
            }
        }
    }
}

/// This impl is necessary to satisfy trait bounds, but
/// [`ZwlrDataControlManager`](ZwlrDataControlManagerV1) has no events, so it can be a no-op.
impl Dispatch<ZwlrDataControlManagerV1, ()> for Clipboard {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlManagerV1,
        _event: <ZwlrDataControlManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &wayland_client::Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

/// The [`ZwlrDataControlDevice`](ZwlrDataControlDeviceV1) emits events when a selection appears or disappears.
/// * The [`DataOffer`](zwlr_data_control_device_v1::Event::DataOffer) event signifies that the Device has data to offer, i.e. that seat has copied
///   something.
/// * The [`Selection`](zwlr_data_control_device_v1::Event::Selection) event is emitted when the seat's selection is set or unset
/// * The [`Finished`](zwlr_data_control_device_v1::Event::Finished) event notifies us that the seat no longer has a valid offer
impl Dispatch<ZwlrDataControlDeviceV1, WlSeat> for Clipboard {
    fn event(
        state: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: <ZwlrDataControlDeviceV1 as Proxy>::Event,
        seat: &WlSeat,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_device_v1::Event::*;
        match event {
            // This event is dispatched to notify us of a Device's Offer. We handle it by preparing
            // a new MIME -> buffer map for the offer.
            DataOffer { id } => {
                debug!("Data offer {id:?} introduced on seat {seat:?}");
                state.offers.insert(id, HashMap::new());
            }
            // This event is dispatched after we have received all the MIME types for the offer
            // indicated, or when the selection is unset
            Selection { id } => {
                if let Some(ref offer) = id {
                    debug!("Selection {offer:?} set on seat {seat:?}");
                } else {
                    debug!("Selection on seat {seat:?} no longer valid");
                }
                let Some(seat) = state.seats.get_mut(seat) else {
                    return;
                };
                // The protocol demands that the previous offer will be destroyed; this will happen
                // automatically because after these calls we no longer hold any references to our
                // proxies.
                seat.offer.as_ref().map(|o| state.offers.remove(o));
                seat.set_offer(id);
            }
            // This event is dispatched to notify us that this device is no longer valid. We need
            // to drop references to it and its offer, if any.
            Finished {} => {
                let Some(seat) = state.seats.get_mut(seat) else {
                    return;
                };
                if let Some(ref offer) = seat.offer {
                    state.offers.remove(offer);
                }
                seat.set_device(None);
                seat.set_offer(None);
            }
            _ => {}
        }
    }

    event_created_child!(Clipboard, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

/// A [`ZwlrDataControlSource`](ZwlrDataControlSourceV1) represents data that another client can
/// request to paste. It receives
/// * The [`Send`](zwlr_data_control_source_v1::Event::Send) event when another client wants to paste
/// * The [`Cancelled`](zwlr_data_control_source_v1::Event::Cancelled) event when another client has
///   taken over the clipboard (i.e., someone else has copied).
impl Dispatch<ZwlrDataControlSourceV1, WlSeat> for Clipboard {
    fn event(
        state: &mut Self,
        source: &ZwlrDataControlSourceV1,
        event: <ZwlrDataControlSourceV1 as Proxy>::Event,
        seat: &WlSeat,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_source_v1::Event::*;
        match event {
            // This event is received when someone wants to paste our data. We are provided with a
            // file descriptor and simply write the data to it.
            Send { mime_type, fd } => state.send(seat, mime_type, fd),
            // This event is received when someone else has copied to indicate that we now don't own the
            // clipboard. We need to grab the clipboard back and receive the new newly copied data.
            Cancelled {} => {
                source.destroy();
                let _ = state.grab();
            }
            _ => (),
        }
    }
}

/// A [`ZwlrDataControlOffer`](ZwlrDataControlOfferV1) represents data that we can request from
/// another client, i.e., something that we can paste. It receives
/// [`Offer`](zwlr_data_control_offer_v1::Event::Offer) events to notify us of which MIME types are
/// available.
impl Dispatch<ZwlrDataControlOfferV1, ()> for Clipboard {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: <ZwlrDataControlOfferV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &wayland_client::Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        use zwlr_data_control_offer_v1::Event::*;
        #[allow(clippy::single_match)]
        match event {
            Offer { mime_type } => {
                debug!("MIME type {mime_type} is available from offer {offer:?}");
                // Create a new pipe through which we can receive the data for this MIME type, then
                // request that data.
                match pipe() {
                    Ok((reader, writer)) => {
                        offer.receive(mime_type.clone(), writer.as_fd());
                        state
                            .offers
                            .get_mut(offer)
                            .map(|o| o.insert(mime_type, reader));
                    }
                    Err(e) => error!("Could not create fipe to receive data: {e}"),
                }
            }
            _ => {}
        }
    }
}
