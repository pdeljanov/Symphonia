// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::audio::{Channels, Layout, SignalSpec};
use sonata_core::errors::{Result, decode_error};
use sonata_core::io::ByteStream;

use super::synthesis;

/// Startng indicies of each scale factor band at various sampling rates for long blocks.
pub const SFB_LONG_BANDS: [[usize; 23]; 9] = [
    // 44.1 kHz, MPEG version 1, derived from ISO/IEC 11172-3 Table B.8
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 52, 62, 74, 90, 110, 134, 162, 196, 238, 288, 342,
        418, 576
    ],
    // 48 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 72, 88, 106, 128, 156, 190, 230, 276, 330,
        384, 576
    ],
    // 32 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 54, 66, 82, 102, 126, 156, 194, 240, 296, 364, 448,
        550, 576
    ],
    // 22.050 kHz, MPEG version 2, derived from ISO/IEC 13818-3 Table B.2
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576
    ],
    // 24 kHz (330 should be 332?)
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 114, 136, 162, 194, 232, 278, 332, 394, 464,
        540, 576
    ],
    // 16 kHz
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576
    ],
    // 11.025 kHz, MPEG version 2.5
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576
    ],
    // 12 kHz
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576
    ],
    // 8 kHz
    [
        0, 12, 24, 36, 48, 60, 72, 88, 108, 132, 160, 192, 232, 280, 336, 400, 476, 566, 568, 570,
        572, 574, 576
    ],
];

/// Starting indicies of each scale factor band at various sampling rates for short blocks. Each
/// value must be multiplied by 3 since there are three equal length windows per short scale factor
/// band.
pub const SFB_SHORT_BANDS: [[usize; 40]; 9] = [
    // 44.1 kHz, MPEG version 1, derived from ISO/IEC 11172-3 Table B.8
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 54, 60, 66, 74, 82, 90, 100, 110, 120,
        132, 144, 156, 170, 184, 198, 216, 234, 252, 274, 296, 318, 348, 378, 408, 464, 520, 576
    ],
    // 48 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 54, 60, 66, 72, 78, 84, 94, 104, 114,
        126, 138, 150, 164, 178, 192, 208, 224, 240, 260, 280, 300, 326, 352, 378, 444, 510, 576
    ],
    // 32 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 54, 60, 66, 74, 82, 90, 102, 114, 126,
        142, 158, 174, 194, 214, 234, 260, 286, 312, 346, 380, 414, 456, 498, 540, 552, 564, 576
    ],
    // 22.050 kHz, MPEG version 2, derived from ISO/IEC 13818-3 Table B.2
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 42, 48, 54, 60, 66, 72, 80, 88, 96, 106, 116, 126,
        140, 154, 168, 186, 204, 222, 248, 274, 300, 332, 364, 396, 438, 480, 522, 540, 558, 576
    ],
    // 24 kHz (330 should be 332?)
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144,
        158, 172, 186, 204, 222, 240, 264, 288, 312, 344, 376, 408, 452, 496, 540, 552, 564, 576
    ],
    // 16 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144,
        158, 172, 186, 204, 222, 240, 264, 288, 312, 342, 372, 402, 442, 482, 522, 540, 558, 576
    ],
    // 11.025 kHz, MPEG version 2.5
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144,
        158, 172, 186, 204, 222, 240, 264, 288, 312, 342, 372, 402, 442, 482, 522, 540, 558, 576
    ],
    // 12 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144,
        158, 172, 186, 204, 222, 240, 264, 288, 312, 342, 372, 402, 442, 482, 522, 540, 558, 576
    ],
    // 8 kHz
    [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 84, 96, 108, 124, 140, 156, 176, 196, 216, 240, 264,
        288, 316, 344, 372, 408, 444, 480, 482, 484, 486, 488, 490, 492, 494, 496, 498, 524, 550,
        576
    ],
];

pub const SFB_MIXED_BANDS: [&'static [usize]; 9] = [
    // 44.1 kHz, MPEG version 1, derived from ISO/IEC 11172-3 Table B.8
    &[
        0, 4, 8, 12, 16, 20, 24, 30,
        36, 40, 44, 48, 54, 60, 66, 74, 82, 90, 100, 110, 120, 132, 144, 156, 170, 184, 198, 216,
        234, 252, 274, 296, 318, 348, 378, 408, 464, 520, 576
    ],
    // 48 kHz
    &[
        0, 4, 8, 12, 16, 20, 24, 30,
        36, 40, 44, 48, 54, 60, 66, 72, 78, 84, 94, 104, 114, 126, 138, 150, 164, 178, 192, 208,
        224, 240, 260, 280, 300, 326, 352, 378, 444, 510, 576
    ],
    // 32 kHz
    &[
        0, 4, 8, 12, 16, 20, 24, 30,
        36, 40, 44, 48, 54, 60, 66, 74, 82, 90, 102, 114, 126, 142, 158, 174, 194, 214, 234, 260,
        286, 312, 346, 380, 414, 456, 498, 540, 552, 564, 576
    ],
    // 22.050 kHz, MPEG version 2, derived from ISO/IEC 13818-3 Table B.2
    &[
        0, 6, 12, 18, 24, 30,
        36, 42, 48, 54, 60, 66, 72, 80, 88, 96, 106, 116, 126, 140, 154, 168, 186, 204, 222, 248,
        274, 300, 332, 364, 396, 438, 480, 522, 540, 558, 576
    ],
    // 24 kHz (330 should be 332?)
    &[
        0, 6, 12, 18, 24, 30,
        36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144, 158, 172, 186, 204, 222, 240, 264,
        288, 312, 344, 376, 408, 452, 496, 540, 552, 564, 576
    ],
    // 16 kHz
    &[
        0, 6, 12, 18, 24, 30,
        36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144, 158, 172, 186, 204, 222, 240, 264,
        288, 312, 342, 372, 402, 442, 482, 522, 540, 558, 576
    ],
    // 11.025 kHz, MPEG version 2.5
    &[
        0, 6, 12, 18, 24, 30,
        36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144, 158, 172, 186, 204, 222, 240, 264,
        288, 312, 342, 372, 402, 442, 482, 522, 540, 558, 576
    ],
    // 12 kHz
    &[
        0, 6, 12, 18, 24, 30,
        36, 42, 48, 54, 62, 70, 78, 88, 98, 108, 120, 132, 144, 158, 172, 186, 204, 222, 240, 264,
        288, 312, 342, 372, 402, 442, 482, 522, 540, 558, 576
    ],
    // 8 kHz
    //
    // Note: The mixed bands for 8kHz do not follow the same pattern as the other sample rates.
    // There does not appear to be a consensus among other MP3 implementations either, so this is
    // at best an educated guess.
    &[
        0, 12, 24,
        36, 40, 44, 48, 56, 64, 72, 84, 96, 108, 124, 140, 156, 176, 196, 216, 240, 264, 288, 316,
        344, 372, 408, 444, 480, 482, 484, 486, 488, 490, 492, 494, 496, 498, 524, 550, 576
    ],
];

pub const SFB_MIXED_SWITCH_POINT: [usize; 9] = [ 8, 8, 8, 6, 6, 6, 6, 6, 3 ];

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

/// For Joint Stereo channel mode, the mode extension describes the features and parameters of the
/// stereo encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum Mode {
    /// Joint Stereo in layer 3 may use both Mid-Side and Intensity encoding.
    Layer3 { mid_side: bool, intensity: bool },
    /// Joint Stereo in layers 1 and 2 may only use Intensity encoding on a set of bands. The range
    /// of bands using intensity encoding is bound..32.
    Intensity { bound: u32 },
}

/// The channel mode.
#[derive(Copy,Clone,Debug,PartialEq)]
pub enum ChannelMode {
    /// Single mono audio channel.
    Mono,
    /// Dual mono audio channels.
    DualMono,
    /// Stereo channels.
    Stereo,
    /// Joint Stereo encoded channels (decodes to Stereo).
    JointStereo(Mode),
}

impl ChannelMode {
    /// Gets the number of channels.
    #[inline(always)]
    pub fn count(&self) -> usize {
        match self {
            ChannelMode::Mono => 1,
            _                 => 2,
        }
    }

    /// Gets the the channel map.
    #[inline(always)]
    pub fn channels(&self) -> Channels {
        match self {
            ChannelMode::Mono => Channels::FRONT_LEFT,
            _                 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
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
    pub channel_mode: ChannelMode,
    pub emphasis: Emphasis,
    pub is_copyrighted: bool,
    pub is_original: bool,
    pub has_padding: bool,
    pub has_crc: bool,
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
        self.channel_mode.count()
    }

    /// Returns true if Intensity Stereo encoding is used, false otherwise.
    #[inline(always)]
    pub fn is_intensity_stereo(&self) -> bool {
        match self.channel_mode {
            ChannelMode::JointStereo(Mode::Intensity { .. }) => true,
            ChannelMode::JointStereo(Mode::Layer3 { intensity, .. }) => intensity,
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
            return decode_error("invalid main_data length, will exceed resevoir buffer");
        }

        // If the offset is less than or equal to the amount of data in the resevoir, shift the
        // re-used bytes to the beginning of the resevoir.
        if main_data_begin <= self.len {
            self.buf.copy_within(self.len - main_data_begin..self.len, 0);
        }
        else {
            // If the offset is greater than the amount of data in the resevoir, then the stream is
            // malformed. However, there are many many ways this could happen. Shift all the data in
            // the resevoir over by the amount of extra bytes expected and then zero the extra bytes.
            eprintln!("mp3: invalid main_data_begin offset.");

            let extra = main_data_begin - self.len;

            self.buf.copy_within(0..self.len, extra);
            for byte in &mut self.buf[0..extra] { *byte = 0 }
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