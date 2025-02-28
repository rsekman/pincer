use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use bounded_integer::BoundedU8;
use nom::{
    branch::alt,
    character::complete::satisfy,
    combinator::{eof, map},
    sequence::terminated,
    IResult, Parser,
};
use serde::{Deserialize, Serialize};

use crate::error::Anyhow;

const N_NUMERIC: u8 = 10;
const N_NAMED: u8 = 26;

pub type NumericT = BoundedU8<0, { N_NUMERIC - 1 }>;
pub type NamedT = BoundedU8<0, { N_NAMED - 1 }>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Ord, PartialOrd, Eq, PartialEq)]
pub enum RegisterAddress {
    Numeric(NumericT),
    Named(NamedT),
}

impl RegisterAddress {
    pub fn iter() -> impl Iterator<Item = Self> {
        Self::iter_numeric().chain(Self::iter_named())
    }

    pub fn iter_numeric() -> impl Iterator<Item = Self> {
        (0u8..N_NUMERIC).map(|n| Self::Numeric(NumericT::new(n).unwrap()))
    }

    pub fn iter_named() -> impl Iterator<Item = Self> {
        (0u8..N_NAMED).map(|n| Self::Named(NamedT::new(n).unwrap()))
    }
}

impl Default for RegisterAddress {
    fn default() -> Self {
        Self::Numeric(NumericT::new(0u8).unwrap())
    }
}
impl Display for RegisterAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        use RegisterAddress::*;
        write!(f, "\"")?;
        match self {
            Numeric(n) => write!(f, "{}", n),
            Named(n) => write!(f, "{}", ascii_shift('a', n.get()).unwrap()),
        }
    }
}

pub const ADDRESS_HELP: &str = "A character from [0-9a-z]";

fn ascii_distance(a: char, b: char) -> Option<u8> {
    u8::try_from((a as u32) - (b as u32)).ok()
}

fn ascii_shift(a: char, b: u8) -> Option<char> {
    u8::try_from((a as u32) + (b as u32))
        .map(|n| n as char)
        .ok()
}

fn do_parse_address(input: &str) -> IResult<&str, RegisterAddress> {
    alt((
        map(satisfy(|c| c.is_ascii_digit()), |c| {
            RegisterAddress::Numeric(ascii_distance(c, '0').and_then(NumericT::new).unwrap())
        }),
        map(satisfy(|c| c.is_ascii_lowercase()), |c| {
            RegisterAddress::Named(ascii_distance(c, 'a').and_then(NamedT::new).unwrap())
        }),
    ))
    .parse(input)
}

impl FromStr for RegisterAddress {
    type Err = Anyhow;
    fn from_str(input: &str) -> Result<RegisterAddress, Anyhow> {
        terminated(do_parse_address, eof)
            .parse(input)
            .map(|(_, addr)| addr)
            .map_err(|_| Anyhow::msg("Register address must be a single character from [0-9a-z]."))
    }
}

pub type MimeType = String;
type DataBuffer = Vec<u8>;
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Register {
    map: HashMap<MimeType, DataBuffer>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RegisterSummary {
    /// The register contains text/plain data, ellipsize but also store the length
    Text(String, usize),
    /// The register contains something else, summarize with its MIME type and length
    Blob(MimeType, usize),
}

impl Register {
    pub fn summarize(&self) -> RegisterSummary {
        RegisterSummary::Text("".to_owned(), 0)
    }

    pub fn clear(&mut self) {
        self.map.clear()
    }

    pub fn insert(&mut self, mime: MimeType, data: DataBuffer) -> Option<DataBuffer> {
        self.map.insert(mime, data)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, mime: &MimeType) -> Option<&DataBuffer> {
        self.map.get(mime)
    }
}

impl<'a> IntoIterator for &'a Register {
    type Item = (&'a MimeType, &'a DataBuffer);
    type IntoIter = std::collections::hash_map::Iter<'a, MimeType, DataBuffer>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl IntoIterator for Register {
    type Item = (MimeType, DataBuffer);
    type IntoIter = std::collections::hash_map::IntoIter<MimeType, DataBuffer>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.into_iter()
    }
}
