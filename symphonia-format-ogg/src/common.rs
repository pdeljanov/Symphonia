// Symphonia
// Copyright (c) 2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub struct OggPacket {
    pub serial: u32,
    pub base_ts: u64,
    pub data: Box<[u8]>,
}
