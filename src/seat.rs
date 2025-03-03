// Mostly copied from wl_clipboard_rs
use std::fmt::{Debug, Display, Formatter};

use serde::{Deserialize, Serialize};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
};

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

/// Struct containing data relating to [`WlSeat`], for internal use by the
/// [`Clipboard`](crate::clipboard::Clipboard)
#[derive(Default)]
pub(crate) struct SeatData {
    /// The name of this seat, if any.
    pub name: Option<String>,

    /// The numeric name assigned to this seat as a gloabl
    pub numeric_name: u32,

    /// The data device of this seat, if any.
    pub device: Option<ZwlrDataControlDeviceV1>,

    /// The data offer of this seat, if any.
    pub offer: Option<ZwlrDataControlOfferV1>,

    /// The primary-selection data offer of this seat, if any.
    pub primary_offer: Option<ZwlrDataControlOfferV1>,
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
    pub fn set_offer(&mut self, new_offer: Option<ZwlrDataControlOfferV1>) {
        let old_offer = self.offer.take();
        self.offer = new_offer;

        if let Some(offer) = old_offer {
            offer.destroy();
        }
    }

    /// Sets this seat's primary-selection data offer.
    ///
    /// Destroys the old one, if any.
    pub fn set_primary_offer(&mut self, new_offer: Option<ZwlrDataControlOfferV1>) {
        let old_offer = self.primary_offer.take();
        self.primary_offer = new_offer;

        if let Some(offer) = old_offer {
            offer.destroy();
        }
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
