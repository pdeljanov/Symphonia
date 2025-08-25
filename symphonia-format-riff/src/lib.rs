// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod common;

#[cfg(feature = "aiff")]
mod aiff;
#[cfg(feature = "wav")]
mod wave;

#[cfg(feature = "aiff")]
pub use aiff::AiffReader;
#[cfg(feature = "wav")]
pub use wave::WavReader;
