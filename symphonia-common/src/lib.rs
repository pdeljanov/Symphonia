// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]
// Build without the standard library when the `std` feature is disabled.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(not(any(feature = "std", feature = "no_std")))]
compile_error!(
    "symphonia-common: enable either the `std` feature (default) or the `no_std` feature"
);

pub mod apple;
pub mod mpeg;
pub mod xiph;
