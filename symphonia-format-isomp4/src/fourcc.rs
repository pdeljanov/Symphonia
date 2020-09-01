// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Four character codes for typical Ftyps (reference: http://ftyps.com/).
#[derive(Debug)]
pub struct FourCc {
    pub val: String,
}

impl FourCc {

    pub fn from_bytes(val: [u8; 4]) -> Option<Self> {
        let mut fourcc = FourCc { val: String::new() };

        for &byte in val.iter() {
            // The characters of a FourCC must be ASCII printable characters.
            if byte < 0x20 || byte > 0x7e {
                return None;
            }
            fourcc.val.push(char::from(byte));
        }

        Some(fourcc)
    }

}
