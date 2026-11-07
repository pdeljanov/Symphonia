//! Opus Decoder
//!
//! The Opus decoder consists of two main blocks: the SILK decoder and
//! the CELT decoder. At any given time, one or both of the SILK and
//! CELT decoders may be active. The output of the Opus decode is the
//! sum of the outputs from the SILK and CELT decoders with proper sample
//! rate conversion and delay compensation on the SILK side, and optional
//! decimation (when decoding to sample rates less than 48 kHz) on the
//! CELT side, as illustrated in the block diagram below.
//! ```text
//!
//!                          +---------+    +------------+
//!                          |  SILK   |    |   Sample   |
//!                       +->| Decoder |--->|    Rate    |----+
//! Bit-    +---------+   |  |         |    | Conversion |    v
//! stream  |  Range  |---+  +---------+    +------------+  /---\  Audio
//! ------->| Decoder |                                     | + |------>
//!         |         |---+  +---------+    +------------+  \---/
//!         +---------+   |  |  CELT   |    | Decimation |    ^
//!                       +->| Decoder |--->| (Optional) |----+
//!                          |         |    |            |
//!                          +---------+    +------------+
//! ```
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4>

use std::sync::LazyLock;

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, unsupported_error};
use symphonia_core::packet::PacketRef;

use crate::entropy;
use crate::packet::FramePacket;
use crate::silk;
use crate::silk::error::Error as SilkError;

/// Static Opus codec info.
static CODEC_INFO: LazyLock<CodecInfo> = LazyLock::new(|| {
    CodecInfo {
        short_name: "opus",
        long_name: "Opus Audio Codec",
        profiles: &[],
    }
});

/// Static supported codecs array.
static SUPPORTED_CODECS: LazyLock<[SupportedAudioCodec; 1]> = LazyLock::new(|| {
    [SupportedAudioCodec {
        id: CODEC_ID_OPUS,
        info: *CODEC_INFO,
    }]
});

/// The `OpusDecoder` struct implements the Symphonia [`AudioDecoder`] trait.
///
/// It owns the top-level codec parameters and output [`AudioBuffer<f32>`], parses
/// RFC 6716 packet framing via `FramePacket`, instantiates the shared per-frame
/// `entropy::Decoder`, and dispatches to the active SILK and CELT layer decoders.
pub struct OpusDecoder {
    params: AudioCodecParameters,
    channels: Channels,
    buffer: AudioBuffer<f32>,
    silk_decoder: silk::Decoder,
}

impl OpusDecoder {
    pub fn try_new(params: &AudioCodecParameters, _options: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_OPUS {
            return unsupported_error("opus: invalid codec");
        }

        let sample_rate = params.sample_rate.ok_or(SilkError::UnsupportedConfig)?;
        let channels = params.channels.clone().ok_or(SilkError::UnsupportedConfig)?;
        let spec = AudioSpec::new(sample_rate, channels.clone());
        let buffer = AudioBuffer::new(spec, sample_rate as usize);
        let mut silk_decoder = silk::Decoder::new();
        silk_decoder.set_output_sample_rate(sample_rate);

        return Ok(Self {
            params: params.to_owned(),
            channels,
            buffer,
            silk_decoder,
        });
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        self.silk_decoder.reset();
        self.buffer.clear();
    }

    fn codec_info(&self) -> &CodecInfo {
        return &CODEC_INFO;
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        return &self.params;
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        self.buffer.clear();

        let frame_packet = FramePacket::new(packet.data)?;
        self.silk_decoder.configure(
            self.channels.clone(),
            frame_packet.frame_size,
            frame_packet.bandwidth,
        )?;

        for frame_data in frame_packet.frames.iter() {
            if frame_data.is_empty() {
                continue;
            }

            let mut range_decoder = entropy::Decoder::new(frame_data)?;
            self.silk_decoder.decode_frame(
                &mut range_decoder,
                frame_packet.stereo,
                &mut self.buffer,
            )?;
        }

        return Ok(self.buffer.as_generic_audio_buffer_ref());
    }

    fn finalize(&mut self) -> FinalizeResult {
        return FinalizeResult::default();
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        return self.buffer.as_generic_audio_buffer_ref();
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        return Ok(Box::new(Self::try_new(params, opts)?));
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        return &SUPPORTED_CODECS[..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::audio::Position;
    use symphonia_core::codecs::audio::well_known::CODEC_ID_VORBIS;

    #[test]
    fn opus_decoder_initialization() {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));

        let mut decoder = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

        assert_eq!(decoder.codec_info().short_name, "opus");
        assert_eq!(decoder.codec_params().codec, CODEC_ID_OPUS);
        assert_eq!(decoder.last_decoded().frames(), 0);
        assert!(decoder.finalize().verify_ok.is_none());

        decoder.reset();

        assert_eq!(OpusDecoder::supported_codecs().len(), 1);
        assert_eq!(OpusDecoder::supported_codecs()[0].id, CODEC_ID_OPUS);
    }

    #[test]
    fn opus_decoder_invalid_codec() {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_VORBIS);

        let result = OpusDecoder::try_new(&params, &AudioDecoderOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn opus_decoder_end_to_end_mono_and_stereo_silk_packets_match_libopus() {
        use symphonia_core::audio::Audio;
        use symphonia_core::units::{Duration, Timestamp};

        // 3 consecutive Mono WideBand 20 ms SILK packets encoded by libopus opus_encode()
        // and decoded at 48 kHz by libopus opus_decode():
        const MONO_WB_PKT_0: [u8; 47] = [
            0x48, 0x82, 0xb6, 0x0a, 0x70, 0x7e, 0xfb, 0xbd, 0xd2, 0x4d, 0x85, 0xcd, 0x78, 0xed,
            0xb6, 0xdf, 0x64, 0x19, 0xe8, 0x13, 0x99, 0x45, 0x17, 0x57, 0xcb, 0xc7, 0x4b, 0xf0,
            0x86, 0xdc, 0x1b, 0xe2, 0x1a, 0xc0, 0xd0, 0x76, 0xa0, 0xd1, 0xbd, 0x0d, 0x17, 0x34,
            0xc7, 0x2b, 0xba, 0x74, 0xa0,
        ];
        const MONO_WB_OUT_0_HEAD: [i16; 24] = [0; 24];
        const MONO_WB_OUT_0_TAIL: [i16; 24] = [
            4426, 4426, 4429, 4427, 4415, 4385, 4336, 4276, 4219, 4182, 4169, 4175, 4181, 4169,
            4133, 4076, 4010, 3948, 3899, 3863, 3837, 3815, 3793, 3767,
        ];

        const MONO_WB_PKT_1: [u8; 73] = [
            0x48, 0xad, 0xe4, 0x00, 0xd2, 0x1d, 0x46, 0x85, 0x7a, 0x36, 0x9b, 0x87, 0xb0, 0x89,
            0xa5, 0xf7, 0xb4, 0xfd, 0x0f, 0x78, 0x05, 0x65, 0x14, 0x71, 0x11, 0x88, 0x36, 0xbc,
            0x0f, 0xcd, 0x35, 0xa1, 0xe4, 0xdc, 0x77, 0xf2, 0xf8, 0x4c, 0x65, 0x3a, 0xb0, 0x69,
            0x4b, 0x94, 0x60, 0xcc, 0xe8, 0x15, 0x48, 0x64, 0xff, 0xec, 0xf3, 0xfb, 0xc2, 0x18,
            0x1f, 0x5d, 0xf2, 0x27, 0x6d, 0x86, 0x26, 0x66, 0x65, 0x5d, 0x41, 0xc7, 0x12, 0xd2,
            0x61, 0x69, 0x62,
        ];
        const MONO_WB_OUT_1_HEAD: [i16; 24] = [
            3732, 3683, 3622, 3558, 3500, 3455, 3421, 3385, 3337, 3273, 3204, 3144, 3105, 3083,
            3071, 3060, 3046, 3031, 3012, 2985, 2947, 2907, 2880, 2877,
        ];
        const MONO_WB_OUT_1_TAIL: [i16; 24] = [
            -1599, -1137, -659, -176, 302, 774, 1248, 1732, 2230, 2738, 3245, 3744, 4232, 4711,
            5186, 5660, 6134, 6601, 7053, 7487, 7900, 8298, 8687, 9067,
        ];

        const MONO_WB_PKT_2: [u8; 27] = [
            0x48, 0x9d, 0x68, 0xf6, 0x16, 0x52, 0xd4, 0x44, 0x2e, 0x8f, 0xec, 0xfa, 0x23, 0xdb,
            0x55, 0xd6, 0x7c, 0x00, 0x4e, 0xe8, 0x89, 0x69, 0x21, 0x5b, 0x61, 0x48, 0x80,
        ];
        const MONO_WB_OUT_2_HEAD: [i16; 24] = [
            9436, 9787, 10117, 10426, 10716, 10994, 11255, 11495, 11707, 11892, 12056, 12204,
            12336, 12448, 12534, 12592, 12630, 12658, 12679, 12683, 12660, 12598, 12503, 12392,
        ];
        const MONO_WB_OUT_2_TAIL: [i16; 24] = [
            -4111, -4172, -4228, -4278, -4322, -4358, -4389, -4415, -4437, -4456, -4472, -4488,
            -4502, -4513, -4518, -4516, -4510, -4501, -4492, -4485, -4483, -4485, -4488, -4490,
        ];

        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));
        let mut mono_decoder =
            OpusDecoder::try_new(&mono_params, &AudioDecoderOptions::default()).unwrap();

        for (pkt_bytes, expected_head, expected_tail) in [
            (&MONO_WB_PKT_0[..], &MONO_WB_OUT_0_HEAD, &MONO_WB_OUT_0_TAIL),
            (&MONO_WB_PKT_1[..], &MONO_WB_OUT_1_HEAD, &MONO_WB_OUT_1_TAIL),
            (&MONO_WB_PKT_2[..], &MONO_WB_OUT_2_HEAD, &MONO_WB_OUT_2_TAIL),
        ] {
            let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from(960u64), pkt_bytes);
            mono_decoder.decode_ref(&pkt).unwrap();
            assert_eq!(mono_decoder.buffer.frames(), 960);
            let ch0 = mono_decoder.buffer.plane(0).unwrap();
            let head_i16: Vec<i16> = ch0[..24]
                .iter()
                .map(|&s| (s * 32768.0).round() as i16)
                .collect();
            let tail_i16: Vec<i16> = ch0[960 - 24..]
                .iter()
                .map(|&s| (s * 32768.0).round() as i16)
                .collect();
            assert_eq!(head_i16.as_slice(), expected_head);
            assert_eq!(tail_i16.as_slice(), expected_tail);
        }

        // 2 consecutive Stereo MediumBand 20 ms SILK packets encoded by libopus opus_encode()
        // and decoded at 48 kHz by libopus opus_decode():
        const STEREO_MB_PKT_0: [u8; 83] = [
            0x2c, 0xa4, 0xd1, 0xb0, 0xc4, 0xb7, 0x87, 0xab, 0x55, 0x78, 0x1c, 0x0b, 0x60, 0x22,
            0xff, 0x6b, 0xf3, 0x0a, 0xae, 0xb0, 0x53, 0x72, 0x31, 0xd3, 0x5a, 0xdb, 0x47, 0x3d,
            0x29, 0x53, 0xd7, 0xcc, 0x19, 0x14, 0x89, 0x5a, 0x36, 0xf2, 0xf7, 0x69, 0xfb, 0x5a,
            0x69, 0xca, 0x00, 0x46, 0x96, 0x77, 0xb4, 0xf1, 0xa5, 0x26, 0x2d, 0xe3, 0x66, 0x13,
            0x9b, 0xb4, 0x1c, 0x8e, 0x34, 0x8f, 0xca, 0x48, 0x87, 0xd8, 0x61, 0x43, 0xf2, 0xe4,
            0xc2, 0xe0, 0x7e, 0x8a, 0x88, 0x82, 0xb6, 0x7d, 0xa8, 0x07, 0xe8, 0x97, 0x58,
        ];
        const STEREO_MB_OUT_L_0_TAIL: [i16; 24] = [
            -2480, -2233, -2000, -1777, -1558, -1337, -1109, -875, -639, -403, -174, 47, 260, 469,
            674, 878, 1079, 1276, 1464, 1645, 1818, 1986, 2151, 2317,
        ];
        const STEREO_MB_OUT_R_0_TAIL: [i16; 24] = [
            1806, 1686, 1557, 1414, 1254, 1080, 899, 718, 543, 370, 194, 7, -193, -405, -620, -830,
            -1029, -1217, -1398, -1583, -1776, -1979, -2189, -2399,
        ];

        const STEREO_MB_PKT_1: [u8; 81] = [
            0x2c, 0xa9, 0x89, 0x67, 0x1d, 0xbe, 0x46, 0x11, 0x74, 0x23, 0x93, 0x7f, 0xec, 0x8b,
            0xb7, 0x10, 0x4e, 0x98, 0xe6, 0x5d, 0x20, 0x79, 0xff, 0x19, 0x92, 0xcf, 0xf5, 0xaa,
            0x90, 0xe3, 0x6b, 0x05, 0x4f, 0x03, 0x95, 0xfc, 0xfc, 0xbc, 0xf8, 0x45, 0xc6, 0x9d,
            0x87, 0x57, 0x02, 0x67, 0x21, 0xba, 0x5f, 0xbf, 0x7a, 0xaa, 0x2e, 0xf4, 0xbc, 0x6e,
            0xd6, 0x3f, 0xb3, 0x34, 0x02, 0x2e, 0x8a, 0xf8, 0x8e, 0x43, 0x6e, 0xfd, 0x81, 0x3f,
            0xd7, 0xf1, 0xd1, 0x97, 0xd2, 0x79, 0x6e, 0x19, 0x0b, 0x30, 0x60,
        ];
        const STEREO_MB_OUT_L_1_TAIL: [i16; 24] = [
            3192, 3168, 3148, 3133, 3123, 3117, 3116, 3120, 3128, 3139, 3149, 3159, 3167, 3174,
            3180, 3185, 3190, 3192, 3190, 3183, 3170, 3154, 3136, 3119,
        ];
        const STEREO_MB_OUT_R_1_TAIL: [i16; 24] = [
            -697, -391, -81, 233, 549, 870, 1191, 1512, 1830, 2145, 2455, 2760, 3061, 3360, 3657,
            3953, 4247, 4535, 4817, 5091, 5355, 5612, 5860, 6102,
        ];

        let mut stereo_params = AudioCodecParameters::new();
        stereo_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));
        let mut stereo_decoder =
            OpusDecoder::try_new(&stereo_params, &AudioDecoderOptions::default()).unwrap();

        for (pkt_bytes, expected_l_tail, expected_r_tail) in [
            (
                &STEREO_MB_PKT_0[..],
                &STEREO_MB_OUT_L_0_TAIL,
                &STEREO_MB_OUT_R_0_TAIL,
            ),
            (
                &STEREO_MB_PKT_1[..],
                &STEREO_MB_OUT_L_1_TAIL,
                &STEREO_MB_OUT_R_1_TAIL,
            ),
        ] {
            let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from(960u64), pkt_bytes);
            stereo_decoder.decode_ref(&pkt).unwrap();
            assert_eq!(stereo_decoder.buffer.frames(), 960);
            let left_tail: Vec<i16> = stereo_decoder.buffer.plane(0).unwrap()[960 - 24..]
                .iter()
                .map(|&s| (s * 32768.0).round() as i16)
                .collect();
            let right_tail: Vec<i16> = stereo_decoder.buffer.plane(1).unwrap()[960 - 24..]
                .iter()
                .map(|&s| (s * 32768.0).round() as i16)
                .collect();
            assert_eq!(left_tail.as_slice(), expected_l_tail);
            assert_eq!(right_tail.as_slice(), expected_r_tail);
        }
    }
}