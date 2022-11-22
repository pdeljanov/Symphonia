// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::AudioBuffer;
use symphonia_core::errors::Result;
use symphonia_core::io::BufReader;

use crate::common::{FrameHeader, Layer};
use crate::synthesis;

pub struct Layer2 {
    pub synthesis: [synthesis::SynthesisState; 2],
}

impl Layer2 {
    pub fn new() -> Self {
        Self { synthesis: Default::default() }
    }
}

impl Layer for Layer2 {
    fn decode(
        &mut self,
        _reader: &mut BufReader<'_>,
        _header: &FrameHeader,
        _out: &mut AudioBuffer<f32>,
    ) -> Result<()> {
        unimplemented!()
    }
}
