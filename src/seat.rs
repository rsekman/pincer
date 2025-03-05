// Mostly copied from wl_clipboard_rs
use std::fmt::{Debug, Display, Formatter};

use serde::{Deserialize, Serialize};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::ZwlrDataControlSourceV1,
};

/// How to identify seats. The protocol guarantees that seats have unique string identifiers.
pub(crate) type SeatIdentifier = String;

/// Enum for which Seat a command should apply to
#[derive(Default, Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub enum SeatSpecification {
    #[default]
    /// The command applies to the first seat encountered
    Unspecified,
    /// The command applies to the seat with this name
    Specified(SeatIdentifier),
}

/// Enum representing the current state of a seat's clipboard
#[derive(Default, Debug, Clone)]
pub(crate) enum ClipboardState {
    #[default]
    /// We have not yet received data about this seat's clipboard from the compositor
    Uninitialized,
    /// This seat doesn't have anything copied
    Unavailable,
    /// Another client owns the clipboard
    Offer(ZwlrDataControlOfferV1),
    /// We own the clipboard
    Source(ZwlrDataControlSourceV1),
}

/// Struct containing data relating to [`WlSeat`](wayland_client::protocol::wl_seat::WlSeat), for
/// internal use by the [`Clipboard`](crate::clipboard::Clipboard)
#[derive(Default)]
pub(crate) struct SeatData {
    /// The name of this seat, if any.
    pub name: Option<String>,

    /// The numeric name assigned to this seat as a global
    pub numeric_name: u32,

    /// The data device of this seat, if any.
    pub device: Option<ZwlrDataControlDeviceV1>,

    /// Current state of this seat's clipboard. Do we or another client own it?
    pub clipboard_state: ClipboardState,
}

impl SeatData {
    /// Sets this seat's name.
    pub fn set_name(&mut self, name: String) {
        self.name = Some(name)
    }

    /// Sets this seat's device.
    ///
    /// Destroys the old one, if any.
    pub fn set_device(&mut self, device: Option<ZwlrDataControlDeviceV1>) {
        let old_device = self.device.take();
        self.device = device;

        if let Some(device) = old_device {
            device.destroy();
        }
    }

    /// Sets this seat's data offer.
    ///
    /// Destroys the old one, if any.
    pub fn set_clipboard_state(&mut self, new_state: ClipboardState) {
        use ClipboardState::*;
        match &self.clipboard_state {
            Offer(offer) => offer.destroy(),
            Source(source) => source.destroy(),
            Uninitialized | Unavailable => {}
        }
        self.clipboard_state = new_state;
    }
}

impl Display for SeatData {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self.name {
            Some(ref name) => write!(f, "{name}"),
            None => write!(f, "[unnamed]"),
        }
    }
}

impl Debug for SeatData {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self.name {
            Some(ref name) => write!(f, "{name}"),
            None => write!(f, "[unnamed]"),
        }
        .and(write!(f, " (#{})", self.numeric_name))
    }
}
