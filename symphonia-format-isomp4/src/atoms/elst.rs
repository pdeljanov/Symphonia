// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result};
use symphonia_core::io::ByteStream;

use crate::atoms::{Atom, AtomHeader};

/// Edit list atom.
#[derive(Debug)]
pub struct ElstAtom {
    header: AtomHeader,
}

impl Atom for ElstAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ByteStream>(_: &mut B, _: AtomHeader) -> Result<Self> {
        todo!();
    }
}