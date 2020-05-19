// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::errors::{Result, decode_error};
use sonata_core::io::{BufStream, ByteStream};

use log::trace;

/// The table-of-contents header (defined in RFC 6716 section 3.1).
#[derive(Copy, Clone)]
pub struct Toc {
    /// Configuration number.
    pub config: u8,
    /// Is stream stereo?
    pub is_stereo: bool,
    /// Number of coded frames.
    pub code: u8,
}

impl Toc {
    pub fn parse(byte: u8) -> Self {
        let config = (byte & 0xf8) >> 3;
        let is_stereo = (byte & 0x04) != 0;
        let code = byte & 0x3;

        Toc { config, is_stereo, code }
    }

    /// The frame size in microseconds.
    pub fn frame_size_us(&self) -> u32 {
        // Look-up table of frame sizes in microseconds. Derived from RFC 6716 Table 2.
        const FRAME_SIZES: [u16; 32] =
        [
            10_000, 20_000, 40_000, 60_000,
            10_000, 20_000, 40_000, 60_000,
            10_000, 20_000, 40_000, 60_000,
            10_000, 20_000, 10_000, 20_000,
             2_500,  5_000, 10_000, 20_000,
             2_500,  5_000, 10_000, 20_000,
             2_500,  5_000, 10_000, 20_000,
             2_500,  5_000, 10_000, 20_000,
        ];

        u32::from(FRAME_SIZES[self.config as usize])
    }
}

/// Read a code 0 packet: one frame per packet.
fn read_code0_packet(toc: Toc, frame_buf: &[u8]) -> Result<()> {
    trace!("\tFrame {{ len={} }}", frame_buf.len());

    Ok(())
}

/// Read a code 1 packet: two equally sized frames per packet.
fn read_code1_packet(toc: Toc, packet_buf: &[u8]) -> Result<()> {
    let len = packet_buf.len();

    // The packet size must be even.
    if len % 2 != 0 {
        return decode_error("invalid code1 packet size");
    }

    let frame0_buf = &packet_buf[..(len / 2)];
    let frame1_buf = &packet_buf[(len / 2)..];

    trace!("\tFrame {{ len={} }}", frame0_buf.len());
    trace!("\tFrame {{ len={} }}", frame1_buf.len());

    Ok(())
}

/// Read a code 2 packet: two frames per packet.
fn read_code2_packet(toc: Toc, packet_buf: &[u8]) -> Result<()> {

    let (frame0_buf, frame1_buf) = match packet_buf[0] {
        0..=251 => {
            let len0 = usize::from(packet_buf[0]);

            if packet_buf.len() - 1 < len0 {
                return decode_error("invalid code2 frame size");
            }

            (&packet_buf[1..1 + len0], &packet_buf[1 + len0..])
        }
        _ => {
            let len0 = (4 * usize::from(packet_buf[1])) + usize::from(packet_buf[0]);

            if packet_buf.len() - 2 < len0 {
                return decode_error("invalid code2 frame size");
            }

            (&packet_buf[2..2 + len0], &packet_buf[2 + len0..])
        }
    };

    trace!("\tFrame {{ len={} }}", frame0_buf.len());
    trace!("\tFrame {{ len={} }}", frame1_buf.len());

    Ok(())
}

/// Read a code 3 packet: an arbitrary number of frames per packet.
fn read_code3_packet(toc: Toc, packet_buf: &[u8]) -> Result<()> {
    let mut reader = BufStream::new(packet_buf);

    let frame_count_byte = reader.read_u8()?;

    let padding_len = if (frame_count_byte & 0x40) != 0 {
        // The number of padding bytes is encoded using 1 or more bytes. If the read byte is 255,
        // then add 254 to the running total and read another byte. If the read byte is < 255, then
        // add it to the running total and exit.
        let mut padding = 0;

        loop {
            match reader.read_u8()? {
                255 => padding += 254,
                n => {
                    padding += u16::from(n);
                    break padding;
                }
            }
        }
    }
    else {
        0
    };

    let is_vbr = (frame_count_byte & 0x80) != 0;
    let n_frames = frame_count_byte & 0x3f;

    // There must be atleast one frame.
    if n_frames == 0 {
        return decode_error("code3 frame count is 0");
    }

    // The audio duration of a packet must not exceed 120ms.
    if u32::from(n_frames) * toc.frame_size_us() > 120_000 {
        return decode_error("code3 total frame length exceeds 120ms");
    }

    trace!("\tCode3 {{ n_frames={}, padding_len={} }}", n_frames, padding_len);

    // VBR packets have the frame lengths recorded individually.
    if is_vbr {
        let mut frame_lens = [0; 48];

        for frame_len in &mut frame_lens[..n_frames as usize] {
            let byte = reader.read_u8()?;

            *frame_len = match byte {
                0..=251 => u16::from(byte),
                _       => u16::from(byte) + (4 * u16::from(reader.read_u8()?)),
            };

            trace!("\t\tFrame {{ len={} }}", *frame_len);
        }
    }
    else {
        let packet_buf = reader.into_remainder();

        let frame_len = (packet_buf.len() - usize::from(padding_len)) / usize::from(n_frames);

        trace!("\t\tFrame {{ len={} }} x {}", frame_len, n_frames);
    }

    Ok(())
}

pub fn read_packet(packet_buf: &[u8]) -> Result<()> {
    // Read TOC.
    let toc = Toc::parse(packet_buf[0]);

    trace!("Packet {{ config={}, is_stereo={}, code={} }}", toc.config, toc.is_stereo, toc.code);

    match toc.code {
        0 => read_code0_packet(toc, &packet_buf[1..]),
        1 => read_code1_packet(toc, &packet_buf[1..]),
        2 => read_code2_packet(toc, &packet_buf[1..]),
        3 => read_code3_packet(toc, &packet_buf[1..]),
        _ => unreachable!(),
    }?;

    Ok(())
}