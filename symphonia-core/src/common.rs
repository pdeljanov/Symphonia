// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `common` module defines common enums, structs, types, etc.

use std::fmt;

/// A four character code.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct FourCc([u8; 4]);

impl FourCc {
    /// Construct a new FourCC code from the given ASCII character byte array.
    ///
    /// # Panics
    ///
    /// Panics if the byte array contains a non-ASCII character.
    pub const fn new(val: [u8; 4]) -> Self {
        assert!(val.is_ascii(), "only ASCII characters are allowed in a FourCc");
        Self(val)
    }

    /// Try to construct a new FourCC code from the given byte array.
    ///
    /// A FourCC cannot contain non-ASCII characters. If a non-ASCII character is found, `None` is
    /// returned.
    pub const fn try_new(val: [u8; 4]) -> Option<Self> {
        if val.is_ascii() {
            Some(Self(val))
        }
        else {
            None
        }
    }

    /// Returns the contained byte array.
    pub const fn get(&self) -> [u8; 4] {
        self.0
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match std::str::from_utf8(&self.0) {
            Ok(name) => f.write_str(name),
            _ => write!(f, "{:x?}", self.0),
        }
    }
}

/// Describes the relative preference of a registered decoder, format reader, or metadata reader if
/// multiple registered implementations support the same codec or format.
#[derive(Copy, Clone)]
pub enum Tier {
    /// Prefer over others.
    Preferred,
    /// Standard tier: neither preferred nor a fallback. Symphonia's first-party decoders and
    /// readers are registered at this level.
    Standard,
    /// Use as a fallback if nothing else is available.
    Fallback,
}