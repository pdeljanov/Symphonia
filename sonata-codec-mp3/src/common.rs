// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::audio::{Layout, SignalSpec};
use sonata_core::errors::{Result, decode_error};
use sonata_core::io::ByteStream;

use super::synthesis;

/// Startng indicies of each scale factor band at various sampling rates for long blocks.
pub const SCALE_FACTOR_LONG_BANDS: [[usize; 23]; 9] = [
    // 44.1 kHz, MPEG version 1, derived from ISO/IEC 11172-3 Table B.8
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 52, 62, 74, 90, 110, 134,
        162, 196, 238, 288, 342, 418, 576,
    ],
    // 48 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 72, 88, 106, 128,
        156, 190, 230, 276, 330, 384, 576,
    ],
    // 32 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 54, 66, 82, 102, 126, 156,
        194, 240, 296, 364, 448, 550, 576,
    ],
    // 22.050 kHz, MPEG version 2, derived from ISO/IEC 13818-3 Table B.2
    [
        0, 4, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 24 kHz (330 should be 332?)
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 114, 136, 162, 194,
        232, 278, 332, 394, 464, 540, 576,
    ],
    // 16 kHz
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 11.025 kHz, MPEG version 2.5
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 12 kHz
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 8 kHz
    [
        0, 12, 24, 36, 48, 60, 72, 88, 108, 132, 160, 192, 232, 280, 336, 400,
        476, 566, 568, 570, 572, 574, 576,
    ],
];

/// Starting indicies of each scale factor band at various sampling rates for short blocks. Each
/// value must be multiplied by 3 since there are three equal length windows per short scale factor
/// band.
pub const SCALE_FACTOR_SHORT_BANDS: [[usize; 14]; 9] = [
    // 44.1 kHz, MPEG version 1
    [ 0, 4, 8, 12, 16, 22, 30, 40,  52,  66,  84, 106, 136, 192 ],
    // 48 kHz
    [ 0, 4, 8, 12, 16, 22, 28, 38,  50,  64,  80, 100, 126, 192 ],
    // 32 kHz
    [ 0, 4, 8, 12, 16, 22, 30, 42,  58,  78, 104, 138, 180, 192 ],
    // 22.050 kHz, MPEG version 2
    [ 0, 4, 8, 12, 18, 24, 32, 42, 56, 74, 100, 132, 174, 192 ],
    // 24 kHz
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 136, 180, 192 ],
    // 16 kHz
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192 ],
    // 11.025 kHz, MPEG version 2.5
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192 ],
    // 12 kHz
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192 ],
    // 8 kHz
    [ 0, 8, 16, 24, 36, 52, 72, 96, 124, 160, 162, 164, 166, 192 ],
];

/// The MPEG audio version.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum MpegVersion {
    /// Version 2.5
    Mpeg2p5,
    /// Version 2
    Mpeg2,
    /// Version 1
    Mpeg1,
}

/// The MPEG audio layer.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum MpegLayer {
    /// Layer 1
    Layer1,
    /// Layer 2
    Layer2,
    /// Layer 3
    Layer3,
}

/// For Joint Stereo mode, the mode extension describes the features and parameters of the Joint
/// Stereo encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum Mode {
    /// Joint Stereo in layer 3 may use both Mid-Side and Intensity encoding.
    Layer3 { mid_side: bool, intensity: bool },
    /// Joint Stereo in layers 1 and 2 may only use Intensity encoding on a set of bands. The range
    /// of bands is [bound..32].
    Intensity { bound: u32 },
}

/// The channel mode.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum Channels {
    /// Single mono audio channel.
    Mono,
    /// Dual mono audio channels.
    DualMono,
    /// Stereo channels.
    Stereo,
    /// Joint Stereo encoded channels (decodes to Stereo).
    JointStereo(Mode),
}

impl Channels {
    /// Gets the number of channels.
    #[inline(always)]
    pub fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            _              => 2,
        }
    }
}

/// The emphasis applied during encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum Emphasis {
    /// No emphasis
    None,
    /// 50/15us
    Fifty15,
    /// CCIT J.17
    CcitJ17,
}

/// A MPEG 1, 2, or 2.5 audio frame header.
#[derive(Debug)]
pub struct FrameHeader {
    pub version: MpegVersion,
    pub layer: MpegLayer,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub sample_rate_idx: usize,
    pub channels: Channels,
    pub emphasis: Emphasis,
    pub is_copyrighted: bool,
    pub is_original: bool,
    pub has_padding: bool,
    pub crc: Option<u16>,
    pub frame_size: usize,
}

impl FrameHeader {
    /// Returns true if this a MPEG1 frame, false otherwise.
    #[inline(always)]
    pub fn is_mpeg1(&self) -> bool {
        self.version == MpegVersion::Mpeg1
    }

    /// Returns true if this a MPEG2.5 frame, false otherwise.
    #[inline(always)]
    pub fn is_mpeg2p5(&self) -> bool {
        self.version == MpegVersion::Mpeg2p5
    }

    /// Returns a signal specification for the frame.
    pub fn spec(&self) -> SignalSpec {
        let layout = match self.n_channels() {
            1 => Layout::Mono,
            2 => Layout::Stereo,
            _ => unreachable!(),
        };

        SignalSpec::new_with_layout(self.sample_rate, layout)
    }

    /// Returns the number of granules in the frame.
    #[inline(always)]
    pub fn n_granules(&self) -> usize {
        match self.version {
            MpegVersion::Mpeg1 => 2,
            _                  => 1,
        }
    }

    /// Returns the number of channels per granule.
    #[inline(always)]
    pub fn n_channels(&self) -> usize {
        self.channels.count()
    }

    /// Returns true if Intensity Stereo encoding is used, false otherwise.
    #[inline(always)]
    pub fn is_intensity_stereo(&self) -> bool {
        match self.channels {
            Channels::JointStereo(Mode::Intensity { .. }) => true,
            Channels::JointStereo(Mode::Layer3 { intensity, .. }) => intensity,
            _ => false,
        }
    }
}

#[derive(Debug,PartialEq)]
pub enum BlockType {
    // Default case when window switching is off. Also the normal case when window switching is
    // on. Granule contains one long block.
    Long,
    Start,
    Short { is_mixed: bool },
    End
}

/// `BitResevoir` implements the bit resevoir mechanism for main_data. Since frames have a
/// deterministic length based on the bit-rate, low-complexity portions of the audio may not need
/// every byte allocated to the frame. The bit resevoir mechanism allows these unused portions of
/// frames to be used by future frames.
pub struct BitResevoir {
    buf: Box<[u8]>,
    len: usize,
}

impl BitResevoir {
    pub fn new() -> Self {
        BitResevoir {
            buf: vec![0u8; 2048].into_boxed_slice(),
            len: 0,
        }
    }

    pub fn fill<B: ByteStream>(
        &mut self,
        reader: &mut B,
        main_data_begin: usize,
        main_data_size: usize
    ) -> Result<()> {

        // The value `main_data_begin` indicates the number of bytes from the previous frames to
        // reuse. It must always be less than or equal to maximum amount of bytes the resevoir can
        // hold taking into account the additional data being added to the resevoir.
        let main_data_end = main_data_begin + main_data_size;

        if main_data_end > self.buf.len() {
            return decode_error("Invalid main_data length, will exceed resevoir buffer.");
        }

        // If the offset is less than or equal to the amount of data in the resevoir, shift the
        // re-used bytes to the beginning of the resevoir.
        if main_data_begin <= self.len {
            self.buf.copy_within(self.len - main_data_begin..self.len, 0);
        }
        else {
            // If the offset is greater than the amount of data in the resevoir, then the stream is
            // technically malformed. However, there are ways this could happen, so simply zero out
            // the resevoir for the length of the offset and pretend things are okay.
            eprintln!("Invalid main_data_begin offset.");
            for byte in &mut self.buf[0..main_data_begin] { *byte = 0 }
        }

        // Read the remaining amount of bytes from the stream into the resevoir.
        reader.read_buf_bytes(&mut self.buf[main_data_begin..main_data_end])?;
        self.len = main_data_end;

        Ok(())
    }

    pub fn bytes_ref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// MP3 depends on the state of the previous frame to decode the next. `State` is a structure
/// containing all the stateful information required to decode the next frame.
pub struct State {
    pub samples: [[[f32; 576]; 2]; 2],
    pub overlap: [[[f32; 18]; 32]; 2],
    pub synthesis: [synthesis::SynthesisState; 2],
    pub resevoir: BitResevoir,
}

impl State {
    pub fn new() -> Self {
        State {
            samples: [[[0f32; 576]; 2]; 2],
            overlap: [[[0f32; 18]; 32]; 2],
            synthesis: Default::default(),
            resevoir: BitResevoir::new(),
        }
    }
}