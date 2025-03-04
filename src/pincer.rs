use std::collections::{BTreeMap, HashMap};

use spdlog::prelude::*;

use crate::error::Error;
use crate::register::{MimeType, NumericT, Register, RegisterAddress, RegisterSummary};
use crate::seat::SeatIdentifier;

/// Struct for the state of the clipboard manager
#[derive(Debug)]
pub struct Pincer {
    // The Pincer's currently active register
    active: Option<RegisterAddress>,
    // To avoid moving data as yanks to "0 shift higher-numbered registers up, we will treat the
    // `numeric` array as a circular buffer. This is the offset in that circular buffer.
    pointer: NumericT,
    numeric: [Register; 10],
    named: [Register; 26],
}

pub type SeatPincerMap = HashMap<SeatIdentifier, Pincer>;

impl Pincer {
    /// Create a new Pincer
    pub fn new() -> Self {
        Pincer {
            active: None,
            pointer: NumericT::new(0).unwrap(),
            numeric: Default::default(),
            named: Default::default(),
        }
    }

    /// Set the Pincer's register pointer
    pub fn set(&mut self, addr: RegisterAddress) {
        self.active = Some(addr);
    }
    /// Get the Pincer's register pointer
    pub fn get(&self) -> Option<RegisterAddress> {
        self.active
    }

    /// Get a register from a register address
    ///
    /// # Arguments
    ///
    /// * `addr` -  address to the register. `None` means the default, `"0`
    pub fn register(&self, addr: Option<RegisterAddress>) -> &Register {
        let addr = addr.unwrap_or_default();
        use RegisterAddress::*;
        match addr {
            Numeric(n) => self
                .numeric
                .get(shift_backward(n, self.pointer).get() as usize),
            Named(n) => self.named.get(n.get() as usize),
        }
        .unwrap()
    }

    pub fn get_active(&self) -> RegisterAddress {
        self.active.unwrap_or_default()
    }

    /// Get the data contained in a register
    ///
    /// # Arguments
    ///
    /// * `addr` -  address to the register to paste from. `None` means the default, `"0`
    /// * `mime` -  MIME type to get
    ///
    /// # Returns
    ///
    /// If the MIME type exists in the register, `Ok(buffer)` where `buffer` contains the data.
    /// Otherwise `Err`.
    pub fn paste_from(
        &self,
        addr: Option<RegisterAddress>,
        mime: &MimeType,
    ) -> Result<&Vec<u8>, Error> {
        let raw_addr = addr.unwrap_or_default();
        self.register(addr).get(mime).ok_or(format!(
            "Register {raw_addr:?} does not contain MIME type {mime}"
        ))
    }

    /// Get data from the currently active register
    ///
    /// # Arguments
    ///
    /// * `mime` -  MIME type to get
    ///
    /// # Returns
    ///
    /// If the MIME type exists in the register, `Ok(buffer)` where `buffer` contains the data.
    /// Otherwise `Err`.
    pub fn paste(&self, mime: &MimeType) -> Result<&Vec<u8>, Error> {
        self.paste_from(self.active, mime)
    }

    fn advance_pointer(&mut self) {
        self.pointer = shift_forward(self.pointer, NumericT::new(1).unwrap())
    }

    /// Yank data of multiple MIME types into a register
    ///
    /// # Arguments
    ///
    /// * `addr` -  address to the register to paste from. `None` means the default, `"0`
    /// * `pastes` - an iterator over `(MIME, buffer)` tuples
    ///
    /// # Returns
    ///
    /// `Ok(n)` where `n` is the total number of bytes yanked if successful,
    /// otherwise `Err`.
    pub fn yank_into<T>(&mut self, addr: Option<RegisterAddress>, pastes: T) -> Result<usize, Error>
    where
        T: Iterator<Item = (MimeType, Vec<u8>)>,
    {
        use RegisterAddress::*;
        let addr = addr.unwrap_or_default();
        if let Numeric(_) = addr {
            self.advance_pointer();
        }
        let reg = match addr {
            Numeric(n) => self
                .numeric
                .get_mut(shift_backward(n, self.pointer).get() as usize),
            Named(n) => self.named.get_mut(n.get() as usize),
        }
        .unwrap();
        reg.clear();
        let mut bytes = 0;
        for (mime, data) in pastes {
            bytes += data.len();
            reg.insert(mime, data);
        }

        debug!("Yanked {bytes} bytes into {addr}");
        Ok(bytes)
    }

    /// Yank data of a single MIME type into a register
    ///
    /// # Arguments
    ///
    /// * `addr` -  address to the register to paste from. `None` means the default, `"0`
    /// * `(mime, data)` - the MIME type of the data and a buffer where it is stored
    ///
    /// # Returns
    ///
    /// `Ok(n)` where `n` is the total number of bytes yanked if successful,
    /// otherwise `Err`.
    pub fn yank_one_into(
        &mut self,
        addr: Option<RegisterAddress>,
        (mime, data): (String, Vec<u8>),
    ) -> Result<usize, Error> {
        self.yank_into(addr, std::iter::once((mime, data)))
    }

    /// Yank data of multiple MIME types into the currently active register
    ///
    /// # Arguments
    ///
    /// * `pastes` - an iterator over `(MIME, buffer)` tuples
    ///
    /// # Returns
    ///
    /// `Ok(n)` where `n` is the total number of bytes yanked if successful,
    /// otherwise `Err`.
    pub fn yank<T>(&mut self, pastes: T) -> Result<usize, Error>
    where
        T: Iterator<Item = (MimeType, Vec<u8>)>,
    {
        self.yank_into(self.active, pastes)
    }

    /// Yank data of a single MIME type into the currently active register
    ///
    /// # Arguments
    ///
    /// * `(mime, data)` - the MIME type of the data and a buffer where it is stored
    ///
    /// # Returns
    ///
    /// `Ok(n)` where `n` is the total number of bytes yanked if successful,
    /// otherwise `Err`.
    pub fn yank_one(&mut self, (mime, data): (String, Vec<u8>)) -> Result<usize, Error> {
        self.yank_one_into(self.active, (mime, data))
    }

    /// Summarize the contents of all registers
    ///
    /// # Returns
    ///
    /// `Ok(m)` where `m` is a map from register addresses to summarise of their contents if
    /// successful, otherwise `Err`.
    pub fn list(&self) -> Result<BTreeMap<RegisterAddress, RegisterSummary>, Error> {
        let mut out = BTreeMap::new();
        out.extend(RegisterAddress::iter().filter_map(|addr| {
            match addr {
                RegisterAddress::Numeric(n) => self.numeric.get(n.get() as usize),
                RegisterAddress::Named(n) => self.named.get(n.get() as usize),
            }
            .and_then(|r| {
                if !r.is_empty() {
                    Some((addr, r.summarize()))
                } else {
                    None
                }
            })
        }));
        Ok(out)
    }
}

impl Default for Pincer {
    fn default() -> Self {
        Self::new()
    }
}

fn shift_forward(x: NumericT, y: NumericT) -> NumericT {
    let z = (x.get() + y.get()).rem_euclid(NumericT::MAX_VALUE + 1);
    NumericT::new(z).unwrap()
}

fn shift_backward(x: NumericT, y: NumericT) -> NumericT {
    let z = (x.get() - y.get()).rem_euclid(NumericT::MAX_VALUE + 1);
    NumericT::new(z).unwrap()
}
