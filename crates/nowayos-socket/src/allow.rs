// SPDX-License-Identifier: MIT OR Apache-2.0
//! The destinations a stack may connect its socket client to: the
//! system's configuration, not the client's choice. A client asks for a
//! connection to any address and port; the stack opens it only if the
//! system description lists both, and otherwise answers
//! [`FailReason::NotAllowed`](crate::FailReason::NotAllowed).
//!
//! The list reaches the stack PD in a read-only region the system
//! description prefills from a file named for its contents -
//! `tcp-allow-A.B.C.D-PORT.bin`, more destinations joined by `+` -
//! which `tools/board-system` writes, so the name is the whole
//! configuration and needs no review of its own. Its layout, 32-bit
//! little-endian words:
//!
//! | Word | Holds |
//! | --- | --- |
//! | 0 | the layout's version, [`ALLOW_LIST_VERSION`] |
//! | 1 | how many destinations follow: 1 to [`MAX_DESTINATIONS`] |
//! | 2 + 2*i | destination *i*'s IPv4 address, its four bytes in order |
//! | 3 + 2*i | its port, 1 to 65535, in the low 16 bits; the high 16 zero |
//!
//! Anything after the last destination is not read: the region is a page
//! of zeros past it. [`AllowList::decode`] refuses a list that breaks a
//! rule - a version or count out of range, an address that cannot be a
//! destination (`is_unicast_destination`), port 0 or bits above it, the
//! same destination twice - so each list has one encoding.

use core::fmt;
use core::net::Ipv4Addr;

use crate::records::is_unicast_destination;

/// The allow list's layout version.
pub const ALLOW_LIST_VERSION: u32 = 1;

/// The most destinations a list holds.
pub const MAX_DESTINATIONS: usize = 8;

/// The longest list in bytes: two words, then two a destination.
pub const ALLOW_LIST_MAX_LEN: usize = 8 + MAX_DESTINATIONS * 8;

/// One destination: an IPv4 address and a TCP port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destination {
    /// The address.
    pub address: Ipv4Addr,
    /// The port.
    pub port: u16,
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.address, self.port)
    }
}

/// Why an allow list was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowListError {
    /// Shorter than its header and destinations.
    TooShort {
        /// The bytes there were.
        len: usize,
    },
    /// Another layout version.
    Version {
        /// The version found.
        found: u32,
    },
    /// A count of destinations of 0 or over [`MAX_DESTINATIONS`].
    Count {
        /// The count found.
        found: u32,
    },
    /// Destination `index`'s address cannot be a TCP destination.
    Address {
        /// Its position in the list.
        index: usize,
    },
    /// Destination `index`'s port word is 0 or has bits above the port.
    Port {
        /// Its position in the list.
        index: usize,
    },
    /// Destination `index` is listed already.
    Duplicate {
        /// Its position in the list.
        index: usize,
    },
}

impl fmt::Display for AllowListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "{len} bytes, too short for its destinations"),
            Self::Version { found } => {
                write!(f, "layout version {found}, not {ALLOW_LIST_VERSION}")
            }
            Self::Count { found } => {
                write!(f, "{found} destinations, not 1 to {MAX_DESTINATIONS}")
            }
            Self::Address { index } => write!(f, "destination {index}'s address cannot be one"),
            Self::Port { index } => write!(f, "destination {index}'s port is 0 or out of range"),
            Self::Duplicate { index } => write!(f, "destination {index} is listed twice"),
        }
    }
}

/// The destinations a stack may connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowList {
    destinations: [Destination; MAX_DESTINATIONS],
    len: usize,
}

impl AllowList {
    /// A list with nothing in it: every connection refused.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            destinations: [Destination {
                address: Ipv4Addr::UNSPECIFIED,
                port: 0,
            }; MAX_DESTINATIONS],
            len: 0,
        }
    }

    /// A list of `destinations`, checked as [`decode`](Self::decode)
    /// checks them: for tests and tools.
    ///
    /// # Errors
    ///
    /// As [`decode`](Self::decode), but for the layout's own fields.
    pub fn new(destinations: &[Destination]) -> Result<Self, AllowListError> {
        let count = destinations.len();
        if count == 0 || count > MAX_DESTINATIONS {
            return Err(AllowListError::Count {
                found: u32::try_from(count).unwrap_or(u32::MAX),
            });
        }
        let mut list = Self::empty();
        for (index, destination) in destinations.iter().enumerate() {
            list.push(index, *destination)?;
        }
        Ok(list)
    }

    /// Decodes the list at the start of `bytes`, by the rules above.
    ///
    /// # Errors
    ///
    /// The first rule the list breaks, as an [`AllowListError`].
    pub fn decode(bytes: &[u8]) -> Result<Self, AllowListError> {
        let word = |index: usize| -> Option<u32> {
            let at = index.checked_mul(4)?;
            let end = at.checked_add(4)?;
            let chunk: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
            Some(u32::from_le_bytes(chunk))
        };
        let too_short = AllowListError::TooShort { len: bytes.len() };
        let version = word(0).ok_or(too_short)?;
        if version != ALLOW_LIST_VERSION {
            return Err(AllowListError::Version { found: version });
        }
        let found = word(1).ok_or(too_short)?;
        let count = usize::try_from(found).unwrap_or(usize::MAX);
        if count == 0 || count > MAX_DESTINATIONS {
            return Err(AllowListError::Count { found });
        }
        let mut list = Self::empty();
        for index in 0..count {
            let at = index.saturating_mul(2).saturating_add(2);
            let address = word(at).ok_or(too_short)?;
            let port = word(at.saturating_add(1)).ok_or(too_short)?;
            let Ok(port) = u16::try_from(port) else {
                return Err(AllowListError::Port { index });
            };
            let address = Ipv4Addr::from(address.to_le_bytes());
            list.push(index, Destination { address, port })?;
        }
        Ok(list)
    }

    /// Adds destination `index`, checked.
    fn push(&mut self, index: usize, destination: Destination) -> Result<(), AllowListError> {
        if !is_unicast_destination(destination.address) {
            return Err(AllowListError::Address { index });
        }
        if destination.port == 0 {
            return Err(AllowListError::Port { index });
        }
        if self.iter().any(|listed| listed == destination) {
            return Err(AllowListError::Duplicate { index });
        }
        let slot = self
            .destinations
            .get_mut(self.len)
            .ok_or(AllowListError::Count {
                found: u32::try_from(self.len.saturating_add(1)).unwrap_or(u32::MAX),
            })?;
        *slot = destination;
        self.len = self.len.saturating_add(1);
        Ok(())
    }

    /// Whether a connection to `address` and `port` is allowed.
    #[must_use]
    pub fn permits(&self, address: Ipv4Addr, port: u16) -> bool {
        self.iter()
            .any(|listed| listed.address == address && listed.port == port)
    }

    /// The destinations, in the list's order.
    pub fn iter(&self) -> impl Iterator<Item = Destination> + '_ {
        self.destinations.iter().take(self.len).copied()
    }

    /// How many destinations the list holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the list holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "test code: a failed check panics, and tests index and compute on known data"
    )]

    use super::*;
    use proptest::prelude::*;

    /// The bytes of a list of `entries`, (address, port word), as
    /// tools/board-system writes it.
    fn encode(version: u32, count: u32, entries: &[([u8; 4], u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(version.to_le_bytes());
        out.extend(count.to_le_bytes());
        for (address, port) in entries {
            out.extend(address);
            out.extend(port.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_list_decodes_and_permits_only_what_it_lists() {
        let bytes = encode(1, 2, &[([10, 0, 2, 100], 7), ([192, 168, 1, 20], 3389)]);
        let list = AllowList::decode(&bytes).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.permits(Ipv4Addr::new(10, 0, 2, 100), 7));
        assert!(list.permits(Ipv4Addr::new(192, 168, 1, 20), 3389));
        assert!(!list.permits(Ipv4Addr::new(10, 0, 2, 100), 8));
        assert!(!list.permits(Ipv4Addr::new(10, 0, 2, 2), 7));
        // A page of zeros after it is not read.
        let mut page = bytes.clone();
        page.resize(4096, 0);
        assert_eq!(AllowList::decode(&page), Ok(list));
        assert_eq!(
            list.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            ["10.0.2.100:7", "192.168.1.20:3389"]
        );
        assert!(!AllowList::empty().permits(Ipv4Addr::UNSPECIFIED, 0));
    }

    #[test]
    fn a_list_that_breaks_a_rule_is_refused() {
        let ok = ([10, 0, 2, 100], 7);
        let cases = [
            (encode(1, 1, &[]), AllowListError::TooShort { len: 8 }),
            (encode(2, 1, &[ok]), AllowListError::Version { found: 2 }),
            (encode(1, 0, &[ok]), AllowListError::Count { found: 0 }),
            (encode(1, 9, &[ok]), AllowListError::Count { found: 9 }),
            (
                encode(1, 1, &[([127, 0, 0, 1], 7)]),
                AllowListError::Address { index: 0 },
            ),
            (
                encode(1, 1, &[([224, 0, 0, 1], 7)]),
                AllowListError::Address { index: 0 },
            ),
            (
                encode(1, 1, &[([10, 0, 2, 100], 0)]),
                AllowListError::Port { index: 0 },
            ),
            (
                encode(1, 1, &[([10, 0, 2, 100], 0x1_0007)]),
                AllowListError::Port { index: 0 },
            ),
            (
                encode(1, 2, &[ok, ok]),
                AllowListError::Duplicate { index: 1 },
            ),
            (Vec::new(), AllowListError::TooShort { len: 0 }),
        ];
        for (bytes, error) in cases {
            assert_eq!(AllowList::decode(&bytes), Err(error), "{bytes:02x?}");
        }
    }

    proptest! {
        #[test]
        fn decoding_never_panics_and_what_it_accepts_keeps_the_rules(
            bytes in proptest::collection::vec(any::<u8>(), 0..ALLOW_LIST_MAX_LEN + 8),
            version_ok in any::<bool>(),
            count in 0u32..10,
        ) {
            let mut bytes = bytes;
            if version_ok && bytes.len() >= 8 {
                bytes[..4].copy_from_slice(&1u32.to_le_bytes());
                bytes[4..8].copy_from_slice(&count.to_le_bytes());
            }
            if let Ok(list) = AllowList::decode(&bytes) {
                prop_assert!((1..=MAX_DESTINATIONS).contains(&list.len()));
                let all: Vec<_> = list.iter().collect();
                for (index, destination) in all.iter().enumerate() {
                    prop_assert!(is_unicast_destination(destination.address));
                    prop_assert!(destination.port != 0);
                    prop_assert!(!all[..index].contains(destination));
                    prop_assert!(list.permits(destination.address, destination.port));
                }
                prop_assert_eq!(AllowList::new(&all), Ok(list));
            }
        }
    }
}
