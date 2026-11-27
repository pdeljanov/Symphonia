// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{Channels, Position, layouts};
use symphonia_core::codecs::CodecProfile;
use symphonia_core::codecs::audio::well_known::profiles::*;
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};

/// MPEG4 audio object types.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioObjectType {
    #[default]
    None,
    Main,
    Lc,
    Ssr,
    Ltp,
    Sbr,
    Scalable,
    TwinVq,
    Celp,
    Hvxc,
    Ttsi,
    MainSynth,
    WavetableSynth,
    GeneralMidi,
    Algorithmic,
    ErAacLc,
    ErAacLtp,
    ErAacScalable,
    ErTwinVq,
    ErBsac,
    ErAacLd,
    ErCelp,
    ErHvxc,
    ErHiln,
    ErParametric,
    Ssc,
    Ps,
    MpegSurround,
    Layer1,
    Layer2,
    Layer3,
    Dst,
    Als,
    Sls,
    SlsNonCore,
    ErAacEld,
    SmrSimple,
    SmrMain,
    Reserved,
    Unknown,
}

impl std::fmt::Display for AudioObjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", AUDIO_OBJECT_TYPE_NAMES[*self as usize])
    }
}

const AUDIO_OBJECT_TYPES: &[AudioObjectType] = &[
    AudioObjectType::None,
    AudioObjectType::Main,
    AudioObjectType::Lc,
    AudioObjectType::Ssr,
    AudioObjectType::Ltp,
    AudioObjectType::Sbr,
    AudioObjectType::Scalable,
    AudioObjectType::TwinVq,
    AudioObjectType::Celp,
    AudioObjectType::Hvxc,
    AudioObjectType::Reserved,
    AudioObjectType::Reserved,
    AudioObjectType::Ttsi,
    AudioObjectType::MainSynth,
    AudioObjectType::WavetableSynth,
    AudioObjectType::GeneralMidi,
    AudioObjectType::Algorithmic,
    AudioObjectType::ErAacLc,
    AudioObjectType::Reserved,
    AudioObjectType::ErAacLtp,
    AudioObjectType::ErAacScalable,
    AudioObjectType::ErTwinVq,
    AudioObjectType::ErBsac,
    AudioObjectType::ErAacLd,
    AudioObjectType::ErCelp,
    AudioObjectType::ErHvxc,
    AudioObjectType::ErHiln,
    AudioObjectType::ErParametric,
    AudioObjectType::Ssc,
    AudioObjectType::Ps,
    AudioObjectType::MpegSurround,
    AudioObjectType::Reserved, // Escape
    AudioObjectType::Layer1,
    AudioObjectType::Layer2,
    AudioObjectType::Layer3,
    AudioObjectType::Dst,
    AudioObjectType::Als,
    AudioObjectType::Sls,
    AudioObjectType::SlsNonCore,
    AudioObjectType::ErAacEld,
    AudioObjectType::SmrSimple,
    AudioObjectType::SmrMain,
];

const AUDIO_OBJECT_TYPE_NAMES: &[&str] = &[
    "None",
    "AAC Main",
    "AAC LC",
    "AAC SSR",
    "AAC LTP",
    "SBR",
    "AAC Scalable",
    "TwinVQ",
    "CELP",
    "HVXC",
    // "(Reserved10)",
    // "(Reserved11)",
    "TTSI",
    "Main synthetic",
    "Wavetable synthesis",
    "General MIDI",
    "Algorithmic Synthesis and Audio FX",
    "ER AAC LC",
    // "(Reserved18)",
    "ER AAC LTP",
    "ER AAC Scalable",
    "ER TwinVQ",
    "ER BSAC",
    "ER AAC LD",
    "ER CELP",
    "ER HVXC",
    "ER HILN",
    "ER Parametric",
    "SSC",
    "PS",
    "MPEG Surround",
    // "(Escape)",
    "Layer-1",
    "Layer-2",
    "Layer-3",
    "DST",
    "ALS",
    "SLS",
    "SLS non-core",
    "ER AAC ELD",
    "SMR Simple",
    "SMR Main",
    "(Reserved)",
    "(Unknown)",
];

/// Try to get the audio object type from the given audio object type index.
pub fn get_mpeg4_audio_object_type_by_index(index: u32) -> Option<AudioObjectType> {
    AUDIO_OBJECT_TYPES.get(index as usize).copied()
}

/// MPEG4 audio sample rate result.
pub enum Mpeg4AudioSampleRate {
    /// The sample rate is known.
    SampleRate(u32),
    /// The sample rate should be read manually.
    ///
    /// For the MPEG4 Audio Specific Config, the sample rate should be read as the next 24-bits
    /// after the sample rate index. For ADTS, this is an error.
    Escape,
    /// The sample rate index was invalid.
    Invalid,
}

/// Try to get the audio sample rate given the sample rate index.
pub fn get_mpeg4_audio_sample_rate_by_index(index: u32) -> Mpeg4AudioSampleRate {
    const MPEG4_AUDIO_SAMPLE_RATES: [u32; 13] =
        [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

    match index {
        0..=12 => Mpeg4AudioSampleRate::SampleRate(MPEG4_AUDIO_SAMPLE_RATES[index as usize]),
        15 => Mpeg4AudioSampleRate::Escape,
        _ => Mpeg4AudioSampleRate::Invalid,
    }
}

/// MPEG4 audio channel configuration result.
pub enum Mpeg4AudioChannels {
    /// The channel configuration is known.
    Channels(Channels),
    /// The channel configuration is defined in-band based on the audio object type (e.g., from the
    /// program configuration element).
    Escape,
    /// The channel configuration index was invalid.
    Invalid,
}

/// Try to get the set of channels given the configuration index.
pub fn get_mpeg4_audio_channels_by_config_index(index: u32) -> Mpeg4AudioChannels {
    let channels = match index {
        0 => return Mpeg4AudioChannels::Escape,
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        3 => layouts::CHANNEL_LAYOUT_AAC_3P0,
        4 => layouts::CHANNEL_LAYOUT_AAC_4P0,
        5 => layouts::CHANNEL_LAYOUT_AAC_5P0,
        6 => layouts::CHANNEL_LAYOUT_AAC_5P1,
        7 => layouts::CHANNEL_LAYOUT_AAC_7P1,
        _ => return Mpeg4AudioChannels::Invalid,
    };
    Mpeg4AudioChannels::Channels(channels)
}

/// MPEG4 Audio Specific Configuration.
#[non_exhaustive]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioSpecificConfig {
    pub object_type: AudioObjectType,
    pub sample_rate: u32,
    pub channels: Option<Channels>,
    /// The position of each channel in bitstream order, if the channels are defined by a program
    /// config element.
    pub channel_order: Option<Vec<Position>>,
    /// The channel elements defined by a program config element.
    pub channel_elements: Option<Vec<PceElement>>,
    pub samples: usize,
    pub sbr_ps_info: Option<(u32, Option<Channels>)>,
    pub sbr_present: bool,
    pub ps_present: bool,
}

impl AudioSpecificConfig {
    /// Read the audio specific configuration from the provided buffer. ISO14496-3-2009
    pub fn read(buf: &[u8]) -> Result<AudioSpecificConfig> {
        let mut bs = BitReaderLtr::new(buf);

        let mut asc = AudioSpecificConfig {
            object_type: Self::read_audio_object_type(&mut bs)?,
            sample_rate: Self::read_sampling_frequency(&mut bs)?,
            ..Default::default()
        };

        if asc.sample_rate == 0 {
            return decode_error("common (mp4a): a sample rate of 0 is invalid");
        }

        asc.channels = Self::read_channel_config(&mut bs)?;

        if (asc.object_type == AudioObjectType::Sbr) || (asc.object_type == AudioObjectType::Ps) {
            asc.sbr_present = true;
            if asc.object_type == AudioObjectType::Ps {
                asc.ps_present = true;
            }
            let ext_srate = Self::read_sampling_frequency(&mut bs)?;
            asc.object_type = Self::read_audio_object_type(&mut bs)?;

            let ext_chans = if asc.object_type == AudioObjectType::ErBsac {
                Self::read_channel_config(&mut bs)?
            }
            else {
                None
            };

            asc.sbr_ps_info = Some((ext_srate, ext_chans));
        }

        match asc.object_type {
            AudioObjectType::Main
            | AudioObjectType::Lc
            | AudioObjectType::Ssr
            | AudioObjectType::Scalable
            | AudioObjectType::TwinVq
            | AudioObjectType::ErAacLc
            | AudioObjectType::ErAacLtp
            | AudioObjectType::ErAacScalable
            | AudioObjectType::ErTwinVq
            | AudioObjectType::ErBsac
            | AudioObjectType::ErAacLd => {
                // GASpecificConfig
                let short_frame = bs.read_bool()?;

                asc.samples = if short_frame { 960 } else { 1024 };

                let depends_on_core = bs.read_bool()?;

                // Some encoders (for example, the one used for Audible audiobooks) set
                // dependsOnCoreCoder but end the config without the coreCoderDelay that should
                // follow. Other decoders read the missing bits as zeros, so do the same and treat
                // the rest of the GASpecificConfig as absent instead of failing.
                let truncated = depends_on_core && bs.bits_left() < 14 + 1;

                if depends_on_core && !truncated {
                    let _delay = bs.read_bits_leq32(14)?;
                }

                let extension_flag = !truncated && bs.read_bool()?;

                if asc.channels.is_none() {
                    let pce = ProgramConfig::read(&mut bs)?;
                    asc.channels = Some(pce.channels);
                    asc.channel_order = Some(pce.order);
                    asc.channel_elements = Some(pce.elements);
                }

                if !truncated
                    && ((asc.object_type == AudioObjectType::Scalable)
                        || (asc.object_type == AudioObjectType::ErAacScalable))
                {
                    let _layer = bs.read_bits_leq32(3)?;
                }

                if extension_flag {
                    if asc.object_type == AudioObjectType::ErBsac {
                        let _num_subframes = bs.read_bits_leq32(5)? as usize;
                        let _layer_length = bs.read_bits_leq32(11)?;
                    }

                    if (asc.object_type == AudioObjectType::ErAacLc)
                        || (asc.object_type == AudioObjectType::ErAacLtp)
                        || (asc.object_type == AudioObjectType::ErAacScalable)
                        || (asc.object_type == AudioObjectType::ErAacLd)
                    {
                        let _section_data_resilience = bs.read_bool()?;
                        let _scalefactors_resilience = bs.read_bool()?;
                        let _spectral_data_resilience = bs.read_bool()?;
                    }

                    let extension_flag3 = bs.read_bool()?;

                    if extension_flag3 {
                        return unsupported_error("common (mp4a): version3 extensions");
                    }
                }
            }
            AudioObjectType::Celp => {
                return unsupported_error("common (mp4a): CELP config");
            }
            AudioObjectType::Hvxc => {
                return unsupported_error("common (mp4a): HVXC config");
            }
            AudioObjectType::Ttsi => {
                return unsupported_error("common (mp4a): TTS config");
            }
            AudioObjectType::MainSynth
            | AudioObjectType::WavetableSynth
            | AudioObjectType::GeneralMidi
            | AudioObjectType::Algorithmic => {
                return unsupported_error("common (mp4a): structured audio config");
            }
            AudioObjectType::ErCelp => {
                return unsupported_error("common (mp4a): ER CELP config");
            }
            AudioObjectType::ErHvxc => {
                return unsupported_error("common (mp4a): ER HVXC config");
            }
            AudioObjectType::ErHiln | AudioObjectType::ErParametric => {
                return unsupported_error("common (mp4a): parametric config");
            }
            AudioObjectType::Ssc => {
                return unsupported_error("common (mp4a): SSC config");
            }
            AudioObjectType::MpegSurround => {
                // bs.ignore_bits(1)?; // sacPayloadEmbedding
                return unsupported_error("common (mp4a): MPEG Surround config");
            }
            AudioObjectType::Layer1 | AudioObjectType::Layer2 | AudioObjectType::Layer3 => {
                return unsupported_error("common (mp4a): MPEG Layer 1/2/3 config");
            }
            AudioObjectType::Dst => {
                return unsupported_error("common (mp4a): DST config");
            }
            AudioObjectType::Als => {
                // bs.ignore_bits(5)?; // fillBits
                return unsupported_error("common (mp4a): ALS config");
            }
            AudioObjectType::Sls | AudioObjectType::SlsNonCore => {
                return unsupported_error("common (mp4a): SLS config");
            }
            AudioObjectType::ErAacEld => {
                return unsupported_error("common (mp4a): ELD config");
            }
            AudioObjectType::SmrSimple | AudioObjectType::SmrMain => {
                return unsupported_error("common (mp4a): symbolic music config");
            }
            _ => {}
        };

        match asc.object_type {
            AudioObjectType::ErAacLc
            | AudioObjectType::ErAacLtp
            | AudioObjectType::ErAacScalable
            | AudioObjectType::ErTwinVq
            | AudioObjectType::ErBsac
            | AudioObjectType::ErAacLd
            | AudioObjectType::ErCelp
            | AudioObjectType::ErHvxc
            | AudioObjectType::ErHiln
            | AudioObjectType::ErParametric
            | AudioObjectType::ErAacEld => {
                let ep_config = bs.read_bits_leq32(2)?;

                if (ep_config == 2) || (ep_config == 3) {
                    return unsupported_error("common (mp4a): error protection config");
                }
                // if ep_config == 3 {
                //     let direct_mapping = bs.read_bit()?;
                //     validate!(direct_mapping);
                // }
            }
            _ => {}
        };

        // Explicit backward-compatible SBR/PS signalling (ISO/IEC 14496-3, 1.6.5). Check for the
        // sync extension after the object type specific configuration regardless of whether the
        // SBR or PS audio object types were used. This handles the common case of an AAC-LC audio
        // object type with an explicit SBR extension (e.g., DAB+).
        if bs.bits_left() >= 16 {
            let sync = bs.read_bits_leq32(11)?;

            if sync == 0x2B7 {
                let ext_otype = Self::read_audio_object_type(&mut bs)?;
                if ext_otype == AudioObjectType::Sbr {
                    asc.sbr_present = bs.read_bool()?;
                    if asc.sbr_present {
                        let ext_srate = Self::read_sampling_frequency(&mut bs)?;
                        asc.sbr_ps_info = Some((ext_srate, None));
                        if bs.bits_left() >= 12 {
                            let sync = bs.read_bits_leq32(11)?;
                            if sync == 0x548 {
                                asc.ps_present = bs.read_bool()?;
                            }
                        }
                    }
                }
                if ext_otype == AudioObjectType::Ps {
                    asc.ps_present = true;
                    asc.sbr_present = bs.read_bool()?;
                    if asc.sbr_present {
                        let ext_srate = Self::read_sampling_frequency(&mut bs)?;
                        asc.sbr_ps_info = Some((ext_srate, None));
                    }
                    let _ext_channels = bs.read_bits_leq32(4)?;
                }
            }
        }

        Ok(asc)
    }

    fn read_audio_object_type<B: ReadBitsLtr>(bs: &mut B) -> Result<AudioObjectType> {
        let index = match bs.read_bits_leq32(5)? {
            index if index < 31 => index as usize,
            31 => (bs.read_bits_leq32(6)? + 32) as usize,
            _ => unreachable!(),
        };

        let aot = AUDIO_OBJECT_TYPES.get(index).copied().unwrap_or(AudioObjectType::Unknown);

        Ok(aot)
    }

    fn read_sampling_frequency<B: ReadBitsLtr>(bs: &mut B) -> Result<u32> {
        let index = bs.read_bits_leq32(4)?;
        let rate = match get_mpeg4_audio_sample_rate_by_index(index) {
            Mpeg4AudioSampleRate::SampleRate(rate) => rate,
            Mpeg4AudioSampleRate::Escape => bs.read_bits_leq32(24)?,
            Mpeg4AudioSampleRate::Invalid => {
                return decode_error("common (mp4a): invalid sample rate");
            }
        };
        Ok(rate)
    }

    fn read_channel_config<B: ReadBitsLtr>(bs: &mut B) -> Result<Option<Channels>> {
        let index = bs.read_bits_leq32(4)?;
        let channels = match get_mpeg4_audio_channels_by_config_index(index) {
            Mpeg4AudioChannels::Channels(channels) => Some(channels),
            Mpeg4AudioChannels::Escape => None,
            Mpeg4AudioChannels::Invalid => {
                return decode_error("common (mp4a): invalid channel configuration");
            }
        };
        Ok(channels)
    }
}

/// A channel element defined by a program config element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PceElement {
    /// The syntactic element ID: 0 for a single channel element, 1 for a channel pair element, or
    /// 3 for a low-frequency effects element.
    pub id: u32,
    /// The element instance tag.
    pub tag: u32,
    /// The index of the first channel of the element in the order of the channels.
    pub first_channel: usize,
    /// The group of the element: 0 for front, 1 for side, 2 for back, or 3 for LFE.
    pub group: u8,
}

/// The channels defined by a program config element (ISO/IEC 14496-3, 4.4.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramConfig {
    /// The set of channels.
    pub channels: Channels,
    /// The position of each channel in bitstream order.
    pub order: Vec<Position>,
    /// The elements, in the order the channels are listed.
    pub elements: Vec<PceElement>,
}

impl ProgramConfig {
    /// Read a program config element, starting at the element instance tag. Reads up to and
    /// including the comment field, which leaves the reader byte-aligned.
    pub fn read<B: ReadBitsLtr>(bs: &mut B) -> Result<ProgramConfig> {
        let _tag = bs.read_bits_leq32(4)?;
        let _object_type = bs.read_bits_leq32(2)?;
        let _sample_rate_index = bs.read_bits_leq32(4)?;

        let num_front = bs.read_bits_leq32(4)?;
        let num_side = bs.read_bits_leq32(4)?;
        let num_back = bs.read_bits_leq32(4)?;
        let num_lfe = bs.read_bits_leq32(2)?;
        let num_assoc = bs.read_bits_leq32(3)?;
        let num_cc = bs.read_bits_leq32(4)?;

        // Mono mixdown, stereo mixdown, and matrix mixdown.
        if bs.read_bool()? {
            bs.ignore_bits(4)?;
        }

        if bs.read_bool()? {
            bs.ignore_bits(4)?;
        }

        if bs.read_bool()? {
            bs.ignore_bits(3)?;
        }

        let mut order = Vec::new();
        let mut elements = Vec::new();
        let mut front_pairs = 0;

        let mut front = Vec::with_capacity(num_front as usize);

        for _ in 0..num_front {
            let is_cpe = bs.read_bool()?;
            let tag = bs.read_bits_leq32(4)?;
            front.push((is_cpe, tag));
        }

        let num_front_cpe = front.iter().filter(|(is_cpe, _)| *is_cpe).count();

        // With a center channel and two channel pairs, the first pair is the one closest to the
        // center, and the second is the outermost.
        let (first_pair, second_pair) = if num_front == 3 {
            (
                [Position::FRONT_LEFT_CENTER, Position::FRONT_RIGHT_CENTER],
                [Position::FRONT_LEFT, Position::FRONT_RIGHT],
            )
        }
        else {
            (
                [Position::FRONT_LEFT, Position::FRONT_RIGHT],
                [Position::FRONT_LEFT_CENTER, Position::FRONT_RIGHT_CENTER],
            )
        };

        for (is_cpe, tag) in front {
            elements.push(PceElement {
                id: u32::from(is_cpe),
                tag,
                first_channel: order.len(),
                group: 0,
            });

            match (is_cpe, front_pairs) {
                (true, 0) => order.extend(first_pair),
                (true, 1) => order.extend(second_pair),
                // Two single channel elements are the left and right channels.
                (false, _) if num_front == 2 && num_front_cpe == 0 => {
                    order.push(if order.is_empty() {
                        Position::FRONT_LEFT
                    }
                    else {
                        Position::FRONT_RIGHT
                    })
                }
                (false, _) if !order.contains(&Position::FRONT_CENTER) => {
                    order.push(Position::FRONT_CENTER)
                }
                _ => return unsupported_error("common (mp4a): front channel element layout"),
            }

            front_pairs += u32::from(is_cpe);
        }

        for _ in 0..num_side {
            let is_cpe = bs.read_bool()?;
            let tag = bs.read_bits_leq32(4)?;
            elements.push(PceElement {
                id: u32::from(is_cpe),
                tag,
                first_channel: order.len(),
                group: 1,
            });

            if !is_cpe || order.contains(&Position::SIDE_LEFT) {
                return unsupported_error("common (mp4a): side channel element layout");
            }

            order.extend([Position::SIDE_LEFT, Position::SIDE_RIGHT]);
        }

        for _ in 0..num_back {
            let is_cpe = bs.read_bool()?;
            let tag = bs.read_bits_leq32(4)?;
            elements.push(PceElement {
                id: u32::from(is_cpe),
                tag,
                first_channel: order.len(),
                group: 2,
            });

            match (is_cpe, order.contains(&Position::REAR_LEFT)) {
                (true, false) => order.extend([Position::REAR_LEFT, Position::REAR_RIGHT]),
                (false, _) if !order.contains(&Position::REAR_CENTER) => {
                    order.push(Position::REAR_CENTER)
                }
                _ => return unsupported_error("common (mp4a): back channel element layout"),
            }
        }

        for _ in 0..num_lfe {
            let tag = bs.read_bits_leq32(4)?;
            elements.push(PceElement { id: 3, tag, first_channel: order.len(), group: 3 });

            if order.contains(&Position::LFE1) {
                return unsupported_error("common (mp4a): multiple lfe channels");
            }

            order.push(Position::LFE1);
        }

        // Associated data and coupling channel elements carry no output channels.
        bs.ignore_bits(num_assoc * 4)?;

        for _ in 0..num_cc {
            bs.ignore_bits(5)?;
        }

        bs.realign();

        let comment_len = bs.read_bits_leq32(8)?;
        bs.ignore_bits(comment_len * 8)?;

        let channels = order.iter().fold(Position::empty(), |acc, &pos| acc | pos);

        if order.is_empty() {
            return decode_error("common (mp4a): program config element has no channels");
        }

        Ok(ProgramConfig { channels: Channels::Positioned(channels), order, elements })
    }
}

pub fn get_audio_codec_profile(asc: &AudioSpecificConfig) -> Option<CodecProfile> {
    match asc.object_type {
        AudioObjectType::Main => Some(CODEC_PROFILE_AAC_MAIN),
        AudioObjectType::Ssr => Some(CODEC_PROFILE_AAC_SSR),
        AudioObjectType::Ltp => Some(CODEC_PROFILE_AAC_LTP),
        AudioObjectType::Lc => {
            if asc.ps_present {
                Some(CODEC_PROFILE_AAC_HE_V2)
            }
            else if asc.sbr_present {
                Some(CODEC_PROFILE_AAC_HE)
            }
            else {
                Some(CODEC_PROFILE_AAC_LC)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_lc_config() {
        // AAC-LC, 44.1 kHz, mono, no core coder.
        let asc = AudioSpecificConfig::read(&[0x12, 0x08]).unwrap();
        assert_eq!(asc.object_type, AudioObjectType::Lc);
        assert_eq!(asc.sample_rate, 44100);
        assert_eq!(asc.channels, Some(layouts::CHANNEL_LAYOUT_MONO));
        assert_eq!(asc.samples, 1024);
    }

    #[test]
    fn read_lc_config_missing_core_coder_delay() {
        // AAC-LC, 44.1 kHz, stereo, with dependsOnCoreCoder set but no coreCoderDelay, as found
        // in Audible audiobooks.
        let asc = AudioSpecificConfig::read(&[0x12, 0x12]).unwrap();
        assert_eq!(asc.object_type, AudioObjectType::Lc);
        assert_eq!(asc.sample_rate, 44100);
        assert_eq!(asc.channels, Some(layouts::CHANNEL_LAYOUT_STEREO));
        assert_eq!(asc.samples, 1024);
    }
}

#[cfg(test)]
mod pce_tests {
    use super::*;

    /// Writes bits, most significant bit first.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, count: usize) {
            for i in (0..count).rev() {
                if self.bits % 8 == 0 {
                    self.bytes.push(0);
                }
                *self.bytes.last_mut().unwrap() |=
                    (((value >> i) & 1) as u8) << (7 - self.bits % 8);
                self.bits += 1;
            }
        }
    }

    /// Write a program config element with the given front, side, back, and LFE elements: for each
    /// of the first three groups, a list of `(is_cpe, tag)`, and for the LFE group, a list of tags.
    fn write_pce(
        front: &[(bool, u32)],
        side: &[(bool, u32)],
        back: &[(bool, u32)],
        lfe: &[u32],
    ) -> Vec<u8> {
        let mut w = BitWriter::default();

        w.put(0, 4); // Element instance tag.
        w.put(1, 2); // Object type.
        w.put(4, 4); // Sampling frequency index.
        w.put(front.len() as u32, 4);
        w.put(side.len() as u32, 4);
        w.put(back.len() as u32, 4);
        w.put(lfe.len() as u32, 2);
        w.put(0, 3); // Associated data elements.
        w.put(0, 4); // Coupling channel elements.
        w.put(0, 3); // Mixdown flags.

        for &(is_cpe, tag) in front.iter().chain(side).chain(back) {
            w.put(u32::from(is_cpe), 1);
            w.put(tag, 4);
        }

        for &tag in lfe {
            w.put(tag, 4);
        }

        w.bits = w.bytes.len() * 8;
        w.put(0, 8); // Comment length.
        w.bytes
    }

    fn read_pce(pce: &[u8]) -> Result<ProgramConfig> {
        ProgramConfig::read(&mut BitReaderLtr::new(pce))
    }

    #[test]
    fn verify_pce_stereo_and_surround() {
        let pce = read_pce(&write_pce(&[(true, 0)], &[], &[], &[])).unwrap();
        assert_eq!(pce.order, [Position::FRONT_LEFT, Position::FRONT_RIGHT]);

        // 5.1: a center, a front pair, a back pair, and an LFE element.
        let pce = read_pce(&write_pce(&[(false, 0), (true, 0)], &[], &[(true, 1)], &[0])).unwrap();
        assert_eq!(
            pce.order,
            [
                Position::FRONT_CENTER,
                Position::FRONT_LEFT,
                Position::FRONT_RIGHT,
                Position::REAR_LEFT,
                Position::REAR_RIGHT,
                Position::LFE1,
            ]
        );

        // The elements are identified by their syntactic element ID and instance tag.
        let ids: Vec<_> = pce.elements.iter().map(|e| (e.id, e.tag, e.first_channel)).collect();
        assert_eq!(ids, [(0, 0, 0), (1, 0, 1), (1, 1, 3), (3, 0, 5)]);
    }

    #[test]
    fn verify_pce_front_pairs_around_a_center() {
        // With a center, the first front pair is the one closest to the center.
        let pce = read_pce(&write_pce(&[(false, 0), (true, 0), (true, 1)], &[], &[], &[])).unwrap();
        assert_eq!(
            pce.order,
            [
                Position::FRONT_CENTER,
                Position::FRONT_LEFT_CENTER,
                Position::FRONT_RIGHT_CENTER,
                Position::FRONT_LEFT,
                Position::FRONT_RIGHT,
            ]
        );
    }

    #[test]
    fn verify_pce_dual_mono() {
        let pce = read_pce(&write_pce(&[(false, 0), (false, 1)], &[], &[], &[])).unwrap();
        assert_eq!(pce.order, [Position::FRONT_LEFT, Position::FRONT_RIGHT]);
    }

    #[test]
    fn verify_pce_rejects_no_channels() {
        assert!(read_pce(&write_pce(&[], &[], &[], &[])).is_err());
    }
}
