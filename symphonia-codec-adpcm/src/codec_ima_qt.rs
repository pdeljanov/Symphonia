// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;

use crate::common::{Nibble, u16_to_i32};
use crate::common_ima::AdpcmImaBlockStatus;

/// Decode a block of 64 frames of a single channel. The 7 least significant bits of the predictor
/// are not stored in the block header. They are carried over from the end of the previous block.
pub(crate) fn decode_block<B: ReadBytes>(
    stream: &mut B,
    buffer: &mut [i32],
    predictor_carry: &mut i32,
) -> Result<()> {
    // IMA4 always uses 34 byte blocks.
    // https://wiki.multimedia.cx/index.php/Apple_QuickTime_IMA_ADPCM
    let header = stream.read_be_u16()?;
    let predictor = u16_to_i32!(header & 0xFF80) | (*predictor_carry & 0x7F);
    let step_index = ((header & 0x7F) as usize).min(88) as i32;

    let mut status = AdpcmImaBlockStatus { predictor, step_index };

    for byte in 0..32 {
        let nibbles = stream.read_u8()?;
        buffer[byte * 2] = status.expand_nibble_bitwise(nibbles, Nibble::Lower);
        buffer[byte * 2 + 1] = status.expand_nibble_bitwise(nibbles, Nibble::Upper);
    }

    *predictor_carry = status.predictor;

    Ok(())
}
