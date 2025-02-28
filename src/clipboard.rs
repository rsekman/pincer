#![allow(unused_imports, unused_variables, dead_code)]
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

use spdlog::prelude::*;

use tokio::select;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use wayland_client::{
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

#[derive(Debug)]
struct State {
    pincers: Arc<Mutex<SeatPincerMap>>,
    seats: HashMap<WlSeat, SeatData>,
    manager: ZwlrDataControlManagerV1,
    offers: HashMap<ZwlrDataControlOfferV1, HashSet<MimeType>>,
}

/// Struct that interfaces with the Wayland compositor
#[derive(Debug)]
pub struct Clipboard {
    queue: EventQueue<State>,
    token: CancellationToken,
    state: State,
}

impl Clipboard {
    pub fn new(
        pincers: Arc<Mutex<SeatPincerMap>>,
        token: CancellationToken,
    ) -> Result<Self, Anyhow> {
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
                    let mut sd = SeatData::default();
                    sd.numeric_name = global.name;
                    (seat, sd)
                })
                .collect()
        });

        let state = State {
            pincers,
            seats,
            manager,
            offers: HashMap::new(),
        };

        Ok(Clipboard {
            queue,
            token,
            state,
        })
    }

    pub fn grab(&mut self) {
        // request each seat's data device
        let qh = self.queue.handle();
        for (seat, data) in &mut self.state.seats {
            let device = self.state.manager.get_data_device(seat, &qh, seat.clone());
            data.set_device(Some(device));
        }
        // round-trip
        let _ = self
            .queue
            .roundtrip(&mut self.state)
            .map_err(|e| error!("Wayland communication error: {e}"));
        // request all the data!
        for (_, data) in &self.state.seats {
            match data.offer.clone() {
                Some(offer) => debug!("Found offer for seat {data:?} {offer:?}"),
                None => debug!("No clipboard for seat {data:?}"),
            }
        }
        // round-trip
        // grab the clipboard
    }
}

impl State {
    pub fn send<T>(&self, seat: &WlSeat, mime: MimeType, fd: T) -> ()
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
}

/// This event is dispatched when a global appears or disappears
impl Dispatch<WlRegistry, GlobalListContents> for State {
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
                    let mut sd = SeatData::default();
                    sd.numeric_name = name;
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
                    let _ = seat_data
                        .name
                        .as_ref()
                        .and_then(|n| pincers.remove(n))
                        .map(|_| ())
                        .ok_or(warn!(
                        "Tried to unmanage clipboard of seat {seat_data:?}, but it was not managed"
                    ));
                }
                state.seats.retain(|_, sd| sd.numeric_name != name);
            }
            _ => {}
        }
    }
}

/// This event is dispatched to notify us of each seat's string name
impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut State,
        seat: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<State>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            let data = state.seats.get_mut(seat).unwrap();
            data.set_name(name.clone());
            let mut pincers = state.pincers.lock().unwrap();
            if !pincers.contains_key(&name) {
                pincers.insert(name, Pincer::new());
            } else {
                warn!("Clipboard of seat {data:?} already managed");
            }
        }
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for State {
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

/// Impl for reading the clipboard
impl Dispatch<ZwlrDataControlDeviceV1, WlSeat> for State {
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
            DataOffer { id } => {
                debug!("Received a data offer {id:?} on seat {seat:?}");
                state.seats.get_mut(seat).map(|d| d.set_offer(Some(id)));
            }
            _ => warn!("TODO: Handle zwlr_data_control_offer_v1 events other than data_offer!"),
        }
    }

    event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlSourceV1, WlSeat> for State {
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
            // This event is received when someone wants to paste
            Send { mime_type, fd } => state.send(seat, mime_type, fd),
            // This event is received when someone else has copied, because now we don't own the
            // clipboard anymore
            Cancelled {} => {
                source.destroy();
            }
            _ => (),
        }
    }
}

// This event is dispatched to tell us which MIME types are available
impl Dispatch<ZwlrDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: <ZwlrDataControlOfferV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &wayland_client::Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            info!("MIME type {mime_type} is available from offer {offer:?}");
            //todo!("Implement saving available MIME types")
        }
    }
}
