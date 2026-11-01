// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]
// TODO: Remove this when refactoring AAC.
#![allow(clippy::needless_range_loop)]
#![cfg_attr(not(feature = "std"), no_std)]

#[macro_use]
extern crate alloc;

#[cfg(not(any(feature = "std", feature = "no_std")))]
compile_error!(
    "symphonia-codec-aac: enable either the `std` feature (default) or the `no_std` feature"
);

mod aac;
mod adts;

pub use aac::AacDecoder;
pub use adts::AdtsReader;
