use std::collections::{BTreeMap, HashMap};

use spdlog::prelude::*;

use crate::error::Error;
use crate::register::{MimeType, NumericT, Register, RegisterAddress, RegisterSummary};
use crate::seat::SeatIdentifier;

/// Struct for the state of the clipboard manager
#[derive(Debug)]
pub struct Pincer {
    active: Option<RegisterAddress>,
    pointer: NumericT,
    numeric: [Register; 10],
    named: [Register; 26],
}

pub type SeatPincerMap = HashMap<SeatIdentifier, Pincer>;

impl Pincer {
    pub fn new() -> Self {
        Pincer {
            active: None,
            pointer: NumericT::new(0).unwrap(),
            numeric: Default::default(),
            named: Default::default(),
        }
    }

    pub fn set(&mut self, addr: RegisterAddress) {
        self.active = Some(addr);
    }
    pub fn get(&self) -> Option<RegisterAddress> {
        self.active
    }

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

    pub fn paste(&self, mime: &MimeType) -> Result<&Vec<u8>, Error> {
        self.paste_from(self.active, mime)
    }

    fn advance_pointer(&mut self) {
        self.pointer = shift_forward(self.pointer, NumericT::new(1).unwrap())
    }

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

    pub fn yank_one_into<T>(
        &mut self,
        addr: Option<RegisterAddress>,
        (mime, data): (String, Vec<u8>),
    ) -> Result<usize, Error> {
        self.yank_into(addr, std::iter::once((mime, data)))
    }

    pub fn yank<T>(&mut self, pastes: T) -> Result<usize, Error>
    where
        T: Iterator<Item = (MimeType, Vec<u8>)>,
    {
        self.yank_into(self.active, pastes)
    }

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

fn shift_forward(x: NumericT, y: NumericT) -> NumericT {
    let z = (x.get() + y.get()).rem_euclid(NumericT::MAX_VALUE + 1);
    NumericT::new(z).unwrap()
}

fn shift_backward(x: NumericT, y: NumericT) -> NumericT {
    let z = (x.get() - y.get()).rem_euclid(NumericT::MAX_VALUE + 1);
    NumericT::new(z).unwrap()
}
