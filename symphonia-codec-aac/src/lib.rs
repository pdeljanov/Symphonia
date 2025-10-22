// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// TODO: Remove this when refactoring AAC.
#![allow(clippy::needless_range_loop)]

mod aac;
mod adts;
mod common;

pub use aac::AacDecoder;
pub use adts::AdtsReader;
