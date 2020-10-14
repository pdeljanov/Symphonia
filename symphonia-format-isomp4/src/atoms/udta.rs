// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ByteStream;
use symphonia_core::meta::MetadataQueue;

use crate::atoms::{Atom, AtomHeader, AtomIterator, AtomType, MetaAtom};

/// User data atom.
pub struct UdtaAtom {
    /// Atom header.
    header: AtomHeader,
    /// Metadata atom.
    pub meta: Option<MetaAtom>,
}

impl UdtaAtom {
    /// Consume any metadata, and push it onto provided `MetadataQueue`.
    pub fn push_metadata(&mut self, queue: &mut MetadataQueue) {
        if let Some(meta) = self.meta.take() {
            meta.consume_metadata(queue);
        }
    }
}

impl Atom for UdtaAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let mut iter = AtomIterator::new(reader, header);
        
        let mut meta = None;

        while let Some(header) = iter.next()? {
            match header.atype {
                AtomType::Meta => {
                    meta = Some(iter.read_atom::<MetaAtom>()?);
                }
                _ => ()
            }
        }

        Ok(UdtaAtom {
            header,
            meta,
        })
    }
}