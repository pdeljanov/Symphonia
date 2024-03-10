// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{Channels, Position};

/// Get the mapping 0 channel listing for the given number of channels.
pub fn vorbis_channels_to_channels(num_channels: u8) -> Option<Channels> {
    let positions = match num_channels {
        1 => Position::FRONT_LEFT,
        2 => Position::FRONT_LEFT | Position::FRONT_RIGHT,
        3 => Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
        4 => {
            Position::FRONT_LEFT
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
        }
        5 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
        }
        6 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
                | Position::LFE1
        }
        7 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::SIDE_LEFT
                | Position::SIDE_RIGHT
                | Position::REAR_CENTER
                | Position::LFE1
        }
        8 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::SIDE_LEFT
                | Position::SIDE_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
                | Position::LFE1
        }
        _ => return None,
    };

    Some(Channels::Positioned(positions))
}
