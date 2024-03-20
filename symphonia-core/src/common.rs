// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `common` module defines common enums, structs, types, etc.

/// Describes the relative preference of a registered decoder, format reader, or metadata reader if
/// multiple registered implementations support the same codec or format.
#[derive(Copy, Clone)]
pub enum Tier {
    /// Prefer over others.
    Preferred,
    /// Standard tier: neither preferred nor a fallback. Symphonia's first-party decoders and
    /// readers are registered at this level.
    Standard,
    /// Use as a fallback if nothing else is available.
    Fallback,
}
