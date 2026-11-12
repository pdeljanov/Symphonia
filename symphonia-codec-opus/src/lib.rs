//! Opus Interactive Audio Codec decoder (`symphonia-codec-opus`)
//! ([RFC 6716](<https://datatracker.ietf.org/doc/html/rfc6716>)).
//!
//! This crate implements a 100% safe Rust decoder for the Opus audio codec as specified in
//! [RFC 6716](<https://datatracker.ietf.org/doc/html/rfc6716>),
//! [RFC 7845](<https://datatracker.ietf.org/doc/html/rfc7845>) (Ogg Opus Encapsulation), and
//! [RFC 8251](<https://datatracker.ietf.org/doc/html/rfc8251>) (Opus Errata and Updates).
//!
//! # Architecture ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>) block diagram)
//!
//! ```text
//!                             +---------+    +------------+
//!                             |  SILK   |    |   Sample   |
//!                          +->| Decoder |--->|    Rate    |----+
//!    Bit-    +---------+   |  |         |    | Conversion |    v
//!    stream  |  Range  |---+  +---------+    +------------+  /---\  Audio
//!    ------->| Decoder |                                     | + |------>
//!            |         |---+  +---------+    +------------+  \---/
//!            +---------+   |  |  CELT   |    | Decimation |    ^
//!                          +->| Decoder |--->| (Optional) |----+
//!                             |         |    |            |
//!                             +---------+    +------------+
//! ```
//!
//! - `toc`: Table of Contents (TOC) byte parser ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
//! - `packet`: Opus packet framing for Codes 0..=3, VBR/CBR, padding, and self-delimited
//!   multi-stream packets ([RFC 6716 Section 3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2>),
//!   [Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)).
//! - `header`: Ogg Opus `OpusHead` identification and `OpusTags` comment headers
//!   ([RFC 7845 Section 5](<https://datatracker.ietf.org/doc/html/rfc7845#section-5>)).
//! - `entropy`: Forward range decoder and backward raw-bit reader
//!   ([RFC 6716 Section 4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>)).
//! - `silk`: SILK linear-predictive low-bitrate speech layer
//!   ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).
//! - `celt`: CELT full-bandwidth MDCT transform audio layer
//!   ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
//! - `decoder`: Top-level [`OpusDecoder`], Hybrid mode synthesis, 5 ms redundant CELT mode
//!   transitions ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)),
//!   and multi-stream surround routing ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
//! - `fixed`: Bit-exact fixed-point primitives of the SILK layer
//!   ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).
//! - `sample_rate`: The five output sample rates and per-frame sample counts
//!   ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
//! - `planes`: The planar `f32` channel pair every decode stage writes into
//!   ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//!
//! # Logging
//!
//! The decoder logs through the `log` crate at two points only: `warn` when it departs from the
//! codec parameters it was given (a declared sample rate it cannot produce is replaced by
//! 48 kHz), and `debug` when it conceals a lost packet. The per-frame decode path never logs.

#![forbid(unsafe_code)]
#![allow(clippy::needless_return)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod celt;
mod decoder;
mod entropy;
mod fixed;
mod header;
mod layout;
mod packet;
mod planes;
mod routing;
mod sample_rate;
mod silk;
mod stream;
#[cfg(test)]
mod test_vectors;
mod toc;

pub use decoder::OpusDecoder;
