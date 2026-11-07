//! SILK Frame Decoder Orchestration
//!
//! Coordinates multi-frame SILK header decoding, optional LBRR frame decoding,
//! regular mid/side SILK frame decoding, and subframe synthesis (RFC 6716 Section 4.2).

use symphonia_core::audio::{Audio, AudioBuffer, Channels};
use symphonia_core::errors::Result;

use crate::entropy;
use crate::silk::{
    CodingMode, DecodedOpusFrame, Frame, LpcOrder, SignalType, SilkChannelFramePair, State,
    SubframeSize,
};
use crate::toc::{Bandwidth, FrameDuration};

/// SILK layer decoder state and frame orchestrator.
#[derive(Debug, Default)]
pub struct Decoder {
    pub(crate) state: State,
}

impl Decoder {
    /// Creates a new SILK layer decoder with default unconfigured state.
    pub fn new() -> Self {
        return Self {
            state: State::default(),
        };
    }

    /// Sets the target API output sample rate (`8000`, `12000`, `16000`, `24000`, or `48000` Hz).
    pub fn set_output_sample_rate(&mut self, output_sample_rate: u32) {
        self.state.output_sample_rate = output_sample_rate;
    }

    /// Resets the SILK decoder state across stream discontinuities.
    pub fn reset(&mut self) {
        self.state.reset();
    }

    /// Reconfigures the SILK decoder state if the packet's frame size, bandwidth, or channel layout changed.
    pub fn configure(
        &mut self,
        channels: Channels,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
    ) -> Result<()> {
        let output_sample_rate = self.state.output_sample_rate;
        self.state
            .configure(channels, frame_size, bandwidth, output_sample_rate)?;

        return Ok(());
    }

    /// Decodes a single Opus frame's SILK layer from `range_decoder`, unmixes Mid-Side stereo
    /// when applicable, resamples to `output_sample_rate`, and appends `f32` PCM to `buffer`.
    pub fn decode_frame(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        is_stereo: bool,
        buffer: &mut AudioBuffer<f32>,
    ) -> Result<()> {
        let api_channels = buffer.spec().channels().count();
        let internal_channels = if is_stereo { 2 } else { 1 };
        let stereo_to_mono = api_channels == 2
            && internal_channels == 1
            && self.state.prev_internal_channels == 2;

        if api_channels == 2 && internal_channels == 2 && self.state.prev_internal_channels == 1 {
            self.state.stereo_state.reset_side();
            self.state.channel_states[1].resampler = self.state.channel_states[0].resampler;
        }
        self.state.prev_internal_channels = internal_channels;

        let decoded_opus_frame = self.decode_opus_frame(range_decoder, is_stereo)?;

        for (f, regular_frame) in decoded_opus_frame.regular_frames.iter().enumerate() {
            self.output_frame_pair(regular_frame, is_stereo, stereo_to_mono && f == 0, buffer)?;
        }

        return Ok(());
    }

    /// Decodes a single Opus frame payload (10, 20, 40, or 60 ms) containing 1 to 3 SILK frames.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>
    pub fn decode_opus_frame(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        is_stereo: bool,
    ) -> Result<DecodedOpusFrame> {
        let header = self.decode_silk_header(range_decoder, is_stereo, self.state.frame_size)?;
        self.state.lbrr_flag = header.has_any_lbrr();

        let silk_frame_duration = State::silk_frame_duration(self.state.frame_size)?;

        let mut lbrr_frames = Vec::new();
        if header.has_any_lbrr() {
            let saved_channel_states = self.state.channel_states;
            let saved_prev_decode_only_middle = self.state.prev_decode_only_middle;
            for ch_state in self.state.channel_states.iter_mut() {
                ch_state.reset();
            }
            self.state.prev_decode_only_middle = false;

            for f in 0..header.num_silk_frames {
                if header.has_frame_lbrr(f) {
                    let mid_coding_mode = if f > 0 && header.per_frame_lbrr[0][f - 1] {
                        CodingMode::CodeConditionally
                    } else {
                        CodingMode::CodeIndependently
                    };
                    let side_coding_mode = if f == 0 || !header.per_frame_lbrr[1][f - 1] {
                        CodingMode::CodeIndependently
                    } else {
                        CodingMode::CodeConditionally
                    };
                    let lbrr_pair = self.decode_silk_frame_pair(
                        range_decoder,
                        silk_frame_duration,
                        is_stereo,
                        header.per_frame_lbrr[0][f],
                        header.per_frame_lbrr[1][f],
                        true,
                        true,
                        mid_coding_mode,
                        side_coding_mode,
                    )?;
                    lbrr_frames.push(lbrr_pair);
                }
            }

            self.state.channel_states = saved_channel_states;
            self.state.prev_decode_only_middle = saved_prev_decode_only_middle;
        }

        let mut regular_frames = Vec::with_capacity(header.num_silk_frames);
        for f in 0..header.num_silk_frames {
            let mid_coding_mode = if f == 0 {
                CodingMode::CodeIndependently
            } else {
                CodingMode::CodeConditionally
            };
            let side_coding_mode = if f == 0 {
                CodingMode::CodeIndependently
            } else if self.state.prev_decode_only_middle {
                CodingMode::CodeIndependentlyNoLtpScaling
            } else {
                CodingMode::CodeConditionally
            };
            let regular_pair = self.decode_silk_frame_pair(
                range_decoder,
                silk_frame_duration,
                is_stereo,
                header.vad_flags[0][f],
                header.vad_flags[1][f],
                header.per_frame_lbrr[0][f] || header.per_frame_lbrr[1][f],
                false,
                mid_coding_mode,
                side_coding_mode,
            )?;
            regular_frames.push(regular_pair);
        }

        return Ok(DecodedOpusFrame {
            header,
            lbrr_frames,
            regular_frames,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode_silk_frame_pair(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        silk_frame_duration: FrameDuration,
        is_stereo: bool,
        mid_vad: bool,
        side_vad: bool,
        lbrr_flag: bool,
        is_lbrr_frame: bool,
        mid_coding_mode: CodingMode,
        side_coding_mode: CodingMode,
    ) -> Result<SilkChannelFramePair> {
        let (stereo_weights, mid_only) = if is_stereo {
            if !is_lbrr_frame || mid_vad {
                let weights = self.decode_stereo_prediction_weights(range_decoder)?;
                let mid_only_flag = self.decode_mid_only_flag(range_decoder, side_vad)?;
                (Some(weights), mid_only_flag)
            } else {
                (None, false)
            }
        } else {
            (None, false)
        };

        let mut mid = if !is_lbrr_frame || mid_vad {
            let mut m = self.decode_single_silk_frame(
                range_decoder,
                0,
                mid_coding_mode,
                silk_frame_duration,
                mid_vad,
                lbrr_flag,
                is_lbrr_frame,
            )?;
            m.stereo_weights = stereo_weights;
            m.mid_only = mid_only;
            m
        } else {
            let sample_count =
                State::calculate_frame_length(self.state.sample_rate, silk_frame_duration)?;
            let num_subframes = SubframeSize::from(silk_frame_duration);
            let lpc_order = LpcOrder::from(self.state.bandwidth);
            Frame::new(sample_count, num_subframes, lpc_order)
        };
        mid.stereo_weights = stereo_weights;
        mid.mid_only = mid_only;

        let decode_side = if is_lbrr_frame {
            is_stereo && side_vad
        } else {
            is_stereo && !mid_only
        };

        let side = if decode_side {
            if !is_lbrr_frame && self.state.prev_decode_only_middle {
                self.state.channel_states[1].reset_after_mid_only();
            }
            let mut side_frame = self.decode_single_silk_frame(
                range_decoder,
                1,
                side_coding_mode,
                silk_frame_duration,
                side_vad,
                lbrr_flag,
                is_lbrr_frame,
            )?;
            side_frame.stereo_weights = stereo_weights;
            side_frame.mid_only = mid_only;
            Some(side_frame)
        } else {
            None
        };

        if is_stereo && !is_lbrr_frame {
            self.state.prev_decode_only_middle = mid_only;
        }

        return Ok(SilkChannelFramePair {
            stereo_weights,
            mid_only,
            mid,
            side,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode_single_silk_frame(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        channel: usize,
        coding_mode: CodingMode,
        silk_frame_duration: FrameDuration,
        vad_flag: bool,
        lbrr_flag: bool,
        is_lbrr_frame: bool,
    ) -> Result<Frame> {
        let sample_count =
            State::calculate_frame_length(self.state.sample_rate, silk_frame_duration)?;
        let num_subframes = SubframeSize::from(silk_frame_duration);
        let lpc_order = LpcOrder::from(self.state.bandwidth);
        let mut frame = Frame::new(sample_count, num_subframes, lpc_order);
        frame.vad_flag = vad_flag;
        frame.lbrr_flag = lbrr_flag;

        frame.frame_type = self.decode_frame_type(range_decoder, vad_flag, is_lbrr_frame)?;

        self.decode_gains(range_decoder, &mut frame, channel, coding_mode)?;

        self.decode_lsf(range_decoder, &mut frame, channel, silk_frame_duration)?;

        if frame.frame_type.signal_type == SignalType::Voiced {
            self.decode_ltp(
                range_decoder,
                &mut frame,
                channel,
                coding_mode,
                silk_frame_duration,
            )?;
        }

        self.state.channel_states[channel].ec_prev_signal_type = frame.frame_type.signal_type;

        self.decode_lcg_seed(range_decoder, &mut frame)?;

        self.decode_excitation(range_decoder, &mut frame)?;

        self.synthesize_channel_frame(&mut frame, channel)?;

        self.state.prev_frame_type = frame.frame_type;
        self.state.channel_states[channel].prev_signal_type = frame.frame_type.signal_type;
        self.state.channel_states[channel].first_frame_after_reset = false;

        return Ok(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::silk::{
        FrameType, QuantizationOffsetType, StereoWeights, constant,
    };
    use symphonia_core::audio::Position;

    fn create_test_silk_decoder(
        stereo: bool,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
    ) -> Decoder {
        let channels = if stereo {
            Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT)
        } else {
            Channels::Positioned(Position::FRONT_LEFT)
        };
        let mut decoder = Decoder::new();
        decoder.configure(channels, frame_size, bandwidth).unwrap();
        return decoder;
    }

    #[test]
    fn silk_header_mono_10ms_and_20ms_flags() {
        // Range-coded by libopus ec_enc:
        // HEADER_MONO_20MS_BYTES = [0x80]: [VAD_0 = 1, LBRR = 0]
        // HEADER_MONO_10MS_LBRR_BYTES = [0x40]: [VAD_0 = 0, LBRR = 1] -> implicit per_frame_lbrr[0][0] = true
        const HEADER_MONO_20MS_BYTES: [u8; 1] = [0x80];
        const HEADER_MONO_10MS_LBRR_BYTES: [u8; 1] = [0x40];

        let decoder = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::NarrowBand);

        let mut dec_20 = entropy::Decoder::new(&HEADER_MONO_20MS_BYTES).unwrap();
        let header = decoder
            .decode_silk_header(&mut dec_20, false, FrameDuration::Ms20)
            .unwrap();

        assert_eq!(header.num_channels, 1);
        assert_eq!(header.num_silk_frames, 1);
        assert_eq!(header.vad_flags[0], [true, false, false]);
        assert!(!header.lbrr_flags[0]);
        assert_eq!(header.per_frame_lbrr[0], [false, false, false]);
        assert!(!header.has_any_lbrr());

        let mut dec_10 = entropy::Decoder::new(&HEADER_MONO_10MS_LBRR_BYTES).unwrap();
        let header_lbrr = decoder
            .decode_silk_header(&mut dec_10, false, FrameDuration::Ms10)
            .unwrap();

        assert!(header_lbrr.lbrr_flags[0]);
        assert_eq!(header_lbrr.per_frame_lbrr[0], [true, false, false]);
        assert!(header_lbrr.has_any_lbrr());
        assert!(header_lbrr.has_frame_lbrr(0));
    }

    #[test]
    fn silk_header_mono_40ms_and_60ms_per_frame_lbrr_masks() {
        // Range-coded by libopus ec_enc:
        // HEADER_MONO_40MS_LBRR_BYTES = [0xa8]: VAD=[1, 0], LBRR=1, Table 4 sym=1 -> mask=2 (0b10)
        // HEADER_MONO_60MS_LBRR_BYTES = [0xd8, 0x80]: VAD=[1, 1, 0], LBRR=1, Table 4 sym=4 -> mask=5 (0b101)
        const HEADER_MONO_40MS_LBRR_BYTES: [u8; 1] = [0xa8];
        const HEADER_MONO_60MS_LBRR_BYTES: [u8; 2] = [0xd8, 0x80];

        let decoder = create_test_silk_decoder(false, FrameDuration::Ms40, Bandwidth::NarrowBand);

        let mut dec_40 = entropy::Decoder::new(&HEADER_MONO_40MS_LBRR_BYTES).unwrap();
        let header_40 = decoder
            .decode_silk_header(&mut dec_40, false, FrameDuration::Ms40)
            .unwrap();

        assert_eq!(header_40.num_silk_frames, 2);
        assert_eq!(header_40.vad_flags[0], [true, false, false]);
        assert_eq!(header_40.per_frame_lbrr[0], [false, true, false]);
        assert!(!header_40.has_frame_lbrr(0));
        assert!(header_40.has_frame_lbrr(1));

        let mut dec_60 = entropy::Decoder::new(&HEADER_MONO_60MS_LBRR_BYTES).unwrap();
        let header_60 = decoder
            .decode_silk_header(&mut dec_60, false, FrameDuration::Ms60)
            .unwrap();

        assert_eq!(header_60.num_silk_frames, 3);
        assert_eq!(header_60.vad_flags[0], [true, true, false]);
        assert_eq!(header_60.per_frame_lbrr[0], [true, false, true]);
        assert!(header_60.has_frame_lbrr(0));
        assert!(!header_60.has_frame_lbrr(1));
        assert!(header_60.has_frame_lbrr(2));
    }

    #[test]
    fn silk_header_stereo_60ms_8_bits_and_dual_channel_lbrr() {
        // Range-coded by libopus ec_enc:
        // Mid: VAD=[1, 0, 1], LBRR=1; Side: VAD=[0, 1, 0], LBRR=1;
        // Mid Table 4 sym=0 -> mask=1 (0b001); Side Table 4 sym=5 -> mask=6 (0b110).
        const HEADER_STEREO_60MS_LBRR_BYTES: [u8; 2] = [0xb5, 0x18];

        let decoder = create_test_silk_decoder(true, FrameDuration::Ms60, Bandwidth::NarrowBand);
        let mut dec = entropy::Decoder::new(&HEADER_STEREO_60MS_LBRR_BYTES).unwrap();
        let header = decoder
            .decode_silk_header(&mut dec, true, FrameDuration::Ms60)
            .unwrap();

        assert_eq!(header.num_channels, 2);
        assert_eq!(header.num_silk_frames, 3);
        assert_eq!(header.vad_flags[0], [true, false, true]);
        assert_eq!(header.vad_flags[1], [false, true, false]);
        assert_eq!(header.lbrr_flags, [true, true]);
        assert_eq!(header.per_frame_lbrr[0], [true, false, false]);
        assert_eq!(header.per_frame_lbrr[1], [false, true, true]);
        assert!(header.has_frame_lbrr(0));
        assert!(header.has_frame_lbrr(1));
        assert!(header.has_frame_lbrr(2));
    }

    #[test]
    fn silk_stereo_prediction_weights_and_mid_only_flag_match_libopus() {
        // Range-coded by libopus ec_enc and verified against silk_stereo_decode_pred & silk_stereo_decode_mid_only:
        // n=7, i0=1, i1=2, i2=2, i3=0 -> w0_Q13 = -6783, w1_Q13 = 1033; mid_only = 1 (tell = 16).
        const STEREO_WEIGHTS_AND_MID_ONLY_BYTES: [u8; 2] = [0x32, 0x1f];

        let decoder = create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::NarrowBand);
        let mut dec = entropy::Decoder::new(&STEREO_WEIGHTS_AND_MID_ONLY_BYTES).unwrap();

        let weights = decoder.decode_stereo_prediction_weights(&mut dec).unwrap();
        assert_eq!(
            weights,
            StereoWeights {
                w0_q13: -6783,
                w1_q13: 1033,
            }
        );

        let tell_before_active_mid_only = dec.tell();
        assert!(!decoder.decode_mid_only_flag(&mut dec, true).unwrap());
        assert_eq!(dec.tell(), tell_before_active_mid_only);

        assert!(decoder.decode_mid_only_flag(&mut dec, false).unwrap());
        assert_eq!(dec.tell(), 16);
    }

    #[test]
    fn silk_multi_frame_40ms_and_lbrr_full_opus_frame_decode() {
        // Range-coded 40 ms Mono NB frame with LBRR on frame 0 and 2 regular 20 ms frames:
        const OPUS_FRAME_40MS_MONO_LBRR_BYTES: [u8; 10] = [
            0x20, 0x04, 0x62, 0x4a, 0xb3, 0x7b, 0x7c, 0x70, 0x97, 0x80,
        ];

        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms40, Bandwidth::NarrowBand);
        let mut dec = entropy::Decoder::new(&OPUS_FRAME_40MS_MONO_LBRR_BYTES).unwrap();
        let decoded = decoder.decode_opus_frame(&mut dec, false).unwrap();

        assert_eq!(decoded.header.num_silk_frames, 2);
        assert_eq!(decoded.lbrr_frames.len(), 1);
        assert_eq!(decoded.regular_frames.len(), 2);

        assert_eq!(decoded.lbrr_frames[0].mid.num_subframes, 4);
        assert_eq!(decoded.lbrr_frames[0].mid.sample_count, 160);
        assert_eq!(decoded.lbrr_frames[0].mid.lsf_interpolation_index, Some(4));

        for reg in decoded.regular_frames.iter() {
            assert_eq!(reg.mid.num_subframes, 4);
            assert_eq!(reg.mid.sample_count, 160);
            assert_eq!(reg.mid.lsf_interpolation_index, Some(4));
        }
    }

    #[test]
    fn silk_gains_independent_delta_clamping_and_log2lin_match_libopus() {
        // Range-coded by libopus ec_enc and verified against silk_gains_dequant:
        // Frame 1 (20ms Voiced, independent): gains_Q16 = [7929856, 4194304, 1048576000, 1686110208], last_idx = 63
        // Frame 2 (10ms Voiced, independent): gains_Q16 = [135266304, 135266304], last_idx = 47
        // Frame 3 (10ms Voiced, conditional): gains_Q16 = [83886080, 83886080], last_idx = 44
        const GAINS_MULTI_FRAME_BYTES: [u8; 6] = [0x4a, 0x94, 0xce, 0xa2, 0xc2, 0xf8];

        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::NarrowBand);
        let mut dec = entropy::Decoder::new(&GAINS_MULTI_FRAME_BYTES).unwrap();

        let mut frame1 = Frame::new(160, 4, 10);
        frame1.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder
            .decode_gains(&mut dec, &mut frame1, 0, CodingMode::CodeIndependently)
            .unwrap();

        assert_eq!(
            frame1.active_subframes().iter().map(|s| s.log_gain).collect::<Vec<_>>(),
            vec![29, 25, 60, 63]
        );
        assert_eq!(
            frame1.active_subframes().iter().map(|s| s.gain_q16).collect::<Vec<_>>(),
            vec![7_929_856, 4_194_304, 1_048_576_000, 1_686_110_208]
        );
        assert_eq!(decoder.state.channel_states[0].last_gain_index, 63);

        let mut frame2 = Frame::new(80, 2, 10);
        frame2.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder
            .decode_gains(&mut dec, &mut frame2, 0, CodingMode::CodeIndependently)
            .unwrap();
        assert_eq!(frame2.subframes[0].log_gain, 47);
        assert_eq!(frame2.subframes[1].log_gain, 47);
        assert_eq!(frame2.subframes[0].gain_q16, 135_266_304);
        assert_eq!(frame2.subframes[1].gain_q16, 135_266_304);

        let mut frame3 = Frame::new(80, 2, 10);
        frame3.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder
            .decode_gains(&mut dec, &mut frame3, 0, CodingMode::CodeConditionally)
            .unwrap();
        assert_eq!(frame3.subframes[0].log_gain, 44);
        assert_eq!(frame3.subframes[1].log_gain, 44);
        assert_eq!(frame3.subframes[0].gain_q16, 83_886_080);
        assert_eq!(frame3.subframes[1].gain_q16, 83_886_080);
    }

    #[test]
    fn silk_lsf_stage1_stage2_backward_prediction_and_stabilization_match_libopus() {
        // Range-coded by libopus ec_enc and verified against silk_NLSF_decode:
        // Stage-2 symbols for i1 = 0 with extensions (+6 at k=8, -5 at k=9):
        const LSF_STAGE2_NB_EXT_BYTES: [u8; 3] = [0xb6, 0x6e, 0x3b];
        const EXPECTED_NLSF_Q15_NB: [i16; 10] = [
            1547, 4508, 7814, 10871, 14261, 17641, 21363, 25184, 26336, 26339,
        ];

        let decoder_nb = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::NarrowBand);
        let mut dec_nb = entropy::Decoder::new(&LSF_STAGE2_NB_EXT_BYTES).unwrap();
        let (d_lpc_nb, res_q10_nb) = decoder_nb.decode_lsf_stage2(&mut dec_nb, 0).unwrap();
        assert_eq!(d_lpc_nb, 10);
        assert_eq!(res_q10_nb[9], -904);
        assert_eq!(res_q10_nb[8], 511);

        let mut nlsf_nb = decoder_nb.reconstruct_nlsf(d_lpc_nb, &res_q10_nb[..d_lpc_nb], 0).unwrap();
        decoder_nb.stabilize_nlsf(&mut nlsf_nb[..d_lpc_nb]).unwrap();
        assert_eq!(&nlsf_nb[..10], &EXPECTED_NLSF_Q15_NB);

        // Test WB (16 coefficients) stabilization with pathological disordered input
        let decoder_wb = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::WideBand);
        let mut pathological_wb = [
            32760, 10, 32700, 5, 20000, 100, 50, 40, 30, 20, 15, 12, 8, 4, 2, 0,
        ];
        decoder_wb.stabilize_nlsf(&mut pathological_wb).unwrap();
        let min_delta_wb = &constant::MINIMUM_SPACING_NORMALIZED_LSF_WB;
        assert!(pathological_wb[0] as i32 >= min_delta_wb[0]);
        for i in 1..16 {
            assert!(
                pathological_wb[i] as i32 - pathological_wb[i - 1] as i32 >= min_delta_wb[i],
                "WB NLSF spacing violated at index {i}"
            );
        }
        assert!((1 << 15) - (pathological_wb[15] as i32) >= min_delta_wb[16]);
    }

    #[test]
    fn silk_lsf_4_subframe_interpolation_matches_libopus() {
        // Range-coded by libopus ec_enc and verified against silk_NLSF_decode:
        const LSF_INTERP_TWO_FRAMES_BYTES: [u8; 4] = [0x13, 0x8d, 0x29, 0x80];
        const EXPECTED_F0_NLSF_Q15: [i16; 10] = [
            1536, 4480, 7680, 10624, 13824, 16896, 20096, 23040, 26368, 29184,
        ];
        const EXPECTED_F1_NLSF_Q15: [i16; 10] = [
            3840, 6272, 9600, 12416, 15488, 18176, 21120, 23808, 26752, 29312,
        ];

        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::NarrowBand);
        let mut dec = entropy::Decoder::new(&LSF_INTERP_TWO_FRAMES_BYTES).unwrap();

        let mut frame0 = Frame::new(160, 4, 10);
        decoder
            .decode_lsf(&mut dec, &mut frame0, 0, FrameDuration::Ms20)
            .unwrap();
        decoder.state.channel_states[0].first_frame_after_reset = false;

        assert_eq!(frame0.lsf_interpolation_index, Some(4));
        for sf in frame0.active_subframes() {
            assert_eq!(sf.nlsf(10), &EXPECTED_F0_NLSF_Q15);
        }

        let mut frame1 = Frame::new(160, 4, 10);
        decoder
            .decode_lsf(&mut dec, &mut frame1, 0, FrameDuration::Ms20)
            .unwrap();

        assert_eq!(frame1.lsf_interpolation_index, Some(2));
        assert_eq!(frame1.subframes[2].nlsf(10), &EXPECTED_F1_NLSF_Q15);
        assert_eq!(frame1.subframes[3].nlsf(10), &EXPECTED_F1_NLSF_Q15);

        for k in 0..10 {
            let expected = EXPECTED_F0_NLSF_Q15[k]
                + (((EXPECTED_F1_NLSF_Q15[k] as i32 - EXPECTED_F0_NLSF_Q15[k] as i32) * 2) >> 2) as i16;
            assert_eq!(frame1.subframes[0].nlsf_q15[k], expected);
            assert_eq!(frame1.subframes[1].nlsf_q15[k], expected);
        }
    }

    #[test]
    fn silk_pitch_lags_nb_mb_wb_absolute_relative_delta_and_ltp_scaling_match_libopus() {
        const LTP_WB_THREE_FRAMES_BYTES: [u8; 8] = [0x39, 0x6f, 0x58, 0xee, 0x9c, 0x60, 0xe0, 0xf4];
        const LTP_MB_10MS_BYTES: [u8; 3] = [0x0b, 0xd1, 0xc0];

        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::WideBand);
        let mut dec_wb = entropy::Decoder::new(&LTP_WB_THREE_FRAMES_BYTES).unwrap();

        // 1. WB 20 ms independent voiced frame
        let mut frame1 = Frame::new(320, 4, 16);
        frame1.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder
            .decode_ltp(&mut dec_wb, &mut frame1, 0, CodingMode::CodeIndependently, FrameDuration::Ms20)
            .unwrap();
        decoder.state.channel_states[0].prev_signal_type = SignalType::Voiced;

        assert_eq!(decoder.state.channel_states[0].prev_lag_index, 43);
        assert_eq!(
            frame1.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![74, 75, 76, 77]
        );
        assert_eq!(frame1.ltp_scale_q14, 8192);
        assert!((frame1.ltp_scale - 0.5).abs() < f32::EPSILON);

        // 2. Conditional voiced frame with relative delta pitch lag
        let mut frame2 = Frame::new(320, 4, 16);
        frame2.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder
            .decode_ltp(&mut dec_wb, &mut frame2, 0, CodingMode::CodeConditionally, FrameDuration::Ms20)
            .unwrap();

        assert_eq!(decoder.state.channel_states[0].prev_lag_index, 46);
        assert_eq!(
            frame2.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![78, 78, 78, 78]
        );
        assert_eq!(frame2.ltp_scale_q14, 15565);

        // 3. Conditional voiced frame with delta_lag_index == 0 falling back to absolute lag
        let mut frame3 = Frame::new(320, 4, 16);
        frame3.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder
            .decode_ltp(&mut dec_wb, &mut frame3, 0, CodingMode::CodeConditionally, FrameDuration::Ms20)
            .unwrap();

        assert_eq!(
            frame3.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![32, 32, 35, 41]
        );

        // 4. MediumBand 10 ms frame (2 subframes, scale = 6, min_lag = 24, max_lag = 216)
        let mut decoder_mb = create_test_silk_decoder(false, FrameDuration::Ms10, Bandwidth::MediumBand);
        let mut dec_mb = entropy::Decoder::new(&LTP_MB_10MS_BYTES).unwrap();
        let mut frame_mb = Frame::new(120, 2, 10);
        frame_mb.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        decoder_mb
            .decode_ltp(&mut dec_mb, &mut frame_mb, 0, CodingMode::CodeIndependently, FrameDuration::Ms10)
            .unwrap();

        assert_eq!(
            frame_mb.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![40, 43]
        );
        assert_eq!(frame_mb.ltp_scale_q14, 12288);
    }

    #[test]
    fn real_libopus_encoded_wb_voiced_packet_matches_c_oracle() {
        // Real 20 ms WideBand Mono Voiced SILK frame encoded by libopus opus_encode()
        // from a 200 Hz harmonic speech-like signal (TOC = 0x48) and verified against
        // C silk_decode_indices + silk_decode_parameters:
        const REAL_LIBOPUS_WB_VOICED_FRAME: [u8; 26] = [
            0x9e, 0xb1, 0x56, 0x16, 0x50, 0xf5, 0x07, 0xbc, 0x1e, 0x76, 0xe6, 0x1d, 0xe3,
            0xe8, 0xf3, 0xe9, 0x8e, 0x07, 0x89, 0xf4, 0x49, 0xff, 0x51, 0x0d, 0x1d, 0x6b,
        ];
        const EXPECTED_NLSF_Q15: [i16; 16] = [
            674, 1292, 2214, 4949, 6390, 9088, 11264, 13184, 15232, 17536, 19712, 21888, 24192,
            26240, 28416, 30336,
        ];

        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::WideBand);
        let mut dec = entropy::Decoder::new(&REAL_LIBOPUS_WB_VOICED_FRAME).unwrap();

        let header = decoder
            .decode_silk_header(&mut dec, false, FrameDuration::Ms20)
            .unwrap();
        assert_eq!(header.vad_flags[0], [true, false, false]);
        assert!(!header.lbrr_flags[0]);

        let mut frame = Frame::new(320, 4, 16);
        frame.vad_flag = header.vad_flags[0][0];
        frame.frame_type = decoder.decode_frame_type(&mut dec, true, false).unwrap();
        assert_eq!(frame.frame_type.signal_type, SignalType::Voiced);
        assert_eq!(frame.frame_type.quantization_offset_type, QuantizationOffsetType::Low);

        decoder
            .decode_gains(&mut dec, &mut frame, 0, CodingMode::CodeIndependently)
            .unwrap();
        assert_eq!(decoder.state.channel_states[0].last_gain_index, 25);
        assert_eq!(
            frame.active_subframes().iter().map(|s| s.gain_q16).collect::<Vec<_>>(),
            vec![4_194_304, 4_194_304, 4_194_304, 4_194_304]
        );

        decoder
            .decode_lsf(&mut dec, &mut frame, 0, FrameDuration::Ms20)
            .unwrap();
        assert_eq!(frame.lsf_interpolation_index, Some(4));
        assert_eq!(frame.subframes[0].nlsf(16), &EXPECTED_NLSF_Q15);

        decoder
            .decode_ltp(&mut dec, &mut frame, 0, CodingMode::CodeIndependently, FrameDuration::Ms20)
            .unwrap();
        assert_eq!(decoder.state.channel_states[0].prev_lag_index, 48);
        assert_eq!(
            frame.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![80, 80, 80, 80]
        );
        assert_eq!(frame.ltp_scale_q14, 15565);

        const REAL_WB_FIRST_32_PCM: [i16; 32] = [
            3, 3, 61, 112, 156, 198, 298, 394, 476, 552, 615, 678, 737, 789, 830, 864, 897, 927,
            947, 963, 967, 958, 944, 919, 886, 845, 797, 749, 695, 639, 579, 520,
        ];
        const REAL_WB_LAST_32_PCM: [i16; 32] = [
            40, -60, -139, -208, -306, -407, -495, -575, -649, -716, -784, -848, -888, -897,
            -918, -948, -957, -968, -1000, -1027, -1054, -1115, -1160, -1192, -1258, -1320,
            -1373, -1421, -1447, -1434, -1389, -1324,
        ];
        const REAL_WB_SLPC_Q14_BUF: [i32; 16] = [
            -245066, -247746, -256088, -262800, -269758, -285468, -296864, -305186, -322048,
            -337976, -351410, -363806, -370326, -366994, -355556, -339060,
        ];

        decoder.decode_lcg_seed(&mut dec, &mut frame).unwrap();
        assert_eq!(frame.lcg_seed, 0);

        decoder.decode_excitation(&mut dec, &mut frame).unwrap();
        assert_eq!(dec.tell(), 209);

        decoder
            .synthesize_channel_frame(&mut frame, 0)
            .unwrap();
        assert_eq!(&frame.output_pcm[..32], &REAL_WB_FIRST_32_PCM);
        assert_eq!(&frame.output_pcm[320 - 32..320], &REAL_WB_LAST_32_PCM);
        assert_eq!(
            &decoder.state.channel_states[0].s_lpc_q14_buf,
            &REAL_WB_SLPC_Q14_BUF
        );
        assert_eq!(decoder.state.channel_states[0].lag_prev, 80);
    }

    #[test]
    fn silk_nlsf2a_nb_wb_and_resonant_stabilization_match_libopus() {
        // Generated from libopus 1.6.1 C silk_NLSF2A and silk_LPC_inverse_pred_gain_c:
        const NLSF_Q15_NB: [i16; 10] = [
            1547, 4508, 7814, 10871, 14261, 17641, 21363, 25184, 26336, 26339,
        ];
        const EXPECTED_A_Q12_NB: [i16; 10] = [
            1545, 716, 1663, -2049, 2207, -1542, 1286, -805, 595, -332,
        ];

        const NLSF_Q15_WB: [i16; 16] = [
            674, 1292, 2214, 4949, 6390, 9088, 11264, 13184, 15232, 17536, 19712, 21888, 24192,
            26240, 28416, 30336,
        ];
        const EXPECTED_A_Q12_WB: [i16; 16] = [
            7772, -3675, 43, -45, 264, -676, 460, -283, 541, -219, 61, -369, 301, -381, 139, 146,
        ];

        const NLSF_Q15_RESONANT: [i16; 10] = [100, 120, 140, 160, 180, 200, 220, 240, 260, 280];
        const EXPECTED_A_Q12_RESONANT: [i16; 10] = [
            16591, -30244, 32672, -23164, 11261, -3802, 880, -134, 12, -1,
        ];

        let a_nb = Decoder::lsf_to_lpc(&NLSF_Q15_NB).unwrap();
        assert_eq!(&a_nb[..10], &EXPECTED_A_Q12_NB);
        assert_eq!(Decoder::lpc_inverse_pred_gain(&a_nb[..10]), 3_238_224);

        let a_wb = Decoder::lsf_to_lpc(&NLSF_Q15_WB).unwrap();
        assert_eq!(&a_wb, &EXPECTED_A_Q12_WB);
        assert_eq!(Decoder::lpc_inverse_pred_gain(&a_wb), 864_548);

        let a_res = Decoder::lsf_to_lpc(&NLSF_Q15_RESONANT).unwrap();
        assert_eq!(&a_res[..10], &EXPECTED_A_Q12_RESONANT);
        assert_eq!(Decoder::lpc_inverse_pred_gain(&a_res[..10]), 386_396);
    }

    #[test]
    fn silk_excitation_shell_splits_lsb_escape_and_signs_match_libopus() {
        // Bitstream encoded by libopus C silk_encode_pulses covering:
        // - block 0: pulse_count = 0 with lsb_count = 1 (escape 17 -> 0, LSB-only pulses + signs)
        // - block 1: pulse_count = 4
        // - block 2: pulse_count = 0, lsb_count = 0 (all-zero block)
        // - block 3: pulse_count = 7 with lsb_count = 2 (double escape 17 -> 17 -> 7)
        // - block 4: pulse_count = 12 (dense shell split)
        const EXCITATION_TEST_BYTES: [u8; 25] = [
            0xa5, 0x77, 0x85, 0xab, 0xf8, 0xf2, 0x89, 0xdb, 0xac, 0xa9, 0x0e, 0xcf, 0xec, 0x43,
            0xbc, 0xb5, 0x5f, 0x16, 0x0f, 0x72, 0xaf, 0xa7, 0x7c, 0xc4, 0x80,
        ];
        const EXPECTED_DECODED_PULSES: [i16; 80] = [
            -1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1, 1, 0, 2, 0, 0, -1, 0, 0, 0, 0, 1,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 2, -3, 4, 1, -2,
            3, 0, -1, 2, 3, 0, 1, 2, -7, -2, 1, 0, 1, -1, 0, -2, 0, -1, 1, 0, 0, -1, 0, -1, 1,
        ];

        let mut decoder =
            create_test_silk_decoder(false, FrameDuration::Ms10, Bandwidth::NarrowBand);
        let mut dec = entropy::Decoder::new(&EXCITATION_TEST_BYTES).unwrap();
        let mut frame = Frame::new(80, 2, 10);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::High);

        decoder.decode_lcg_seed(&mut dec, &mut frame).unwrap();
        assert_eq!(frame.lcg_seed, 2);

        decoder.decode_excitation(&mut dec, &mut frame).unwrap();
        assert_eq!(dec.tell(), 193);
        assert_eq!(&frame.pulses[..80], &EXPECTED_DECODED_PULSES);
    }

    #[test]
    fn silk_stereo_ms_to_lr_and_resampler_modes_match_libopus() {
        use crate::silk::resampler::ResamplerState;

        const EXPECTED_STEREO_LEFT: [i16; 16] = [
            -594, -1014, 1478, -861, 406, -183, -967, 1568, -465, 673, -3352, 3939, -1276, 1470,
            -5677, 6250,
        ];
        const EXPECTED_STEREO_RIGHT: [i16; 16] = [
            -246, -1386, 288, -271, 92, 319, 197, -164, -1573, 1999, 46, 1, -3298, 3738, -165, 226,
        ];
        const EXPECTED_RESAMP_8_TO_16_TAIL: [i16; 16] = [
            3819, 4356, 4647, 4591, 4133, 3279, 2094, 701, -745, -2085, -3176, -3920, -4275,
            -4262, -3957, -3473,
        ];
        const EXPECTED_RESAMP_16_TO_8_TAIL: [i16; 16] = [
            4383, 829, -468, -1852, -4765, -3335, 1948, 3713, 2445, 2516, 728, -4079, -4964,
            -1025, 1123, 1766,
        ];
        const EXPECTED_RESAMP_16_TO_12_TAIL: [i16; 16] = [
            -2456, 1230, 3540, 3513, 2585, 2376, 2525, 1311, -1750, -4749, -5277, -3110, -368,
            997, 1279, 2097,
        ];

        let mut decoder = create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::MediumBand);
        decoder.state.stereo_state.pred_prev_q13 = [-1524, 2816];
        decoder.state.stereo_state.s_mid = [310, -420];
        decoder.state.stereo_state.s_side = [95, -110];

        let mut x_mid = [0i16; 18];
        let mut x_side = [0i16; 18];
        for i in 0..16 {
            let sign_m = if (i & 1) != 0 { -1 } else { 1 };
            let sign_s = if (i & 2) != 0 { 1 } else { -1 };
            x_mid[i + 2] = ((i as i32 * 317 - 1200) * sign_m) as i16;
            x_side[i + 2] = ((i as i32 * 193 - 500) * sign_s) as i16;
        }
        Decoder::unmix_mid_side_to_left_right(
            &mut decoder.state.stereo_state,
            &mut x_mid,
            &mut x_side,
            crate::silk::StereoWeights {
                w0_q13: 3120,
                w1_q13: -1840,
            },
            12,
            16,
        );
        assert_eq!(&x_mid[1..17], &EXPECTED_STEREO_LEFT);
        assert_eq!(&x_side[1..17], &EXPECTED_STEREO_RIGHT);

        let mut in_sig = [0i16; 80];
        for (i, sample) in in_sig.iter_mut().enumerate() {
            let v = 4000.0 * (2.0 * std::f64::consts::PI * i as f64 / 16.0).sin()
                + 1500.0 * (2.0 * std::f64::consts::PI * i as f64 / 7.0).cos();
            *sample = v as i16;
        }

        let mut res_up2 = ResamplerState::default();
        res_up2.init(8000, 16000).unwrap();
        let mut out_up2 = [0i16; 160];
        res_up2.resample(&mut out_up2, &in_sig).unwrap();
        assert_eq!(&out_up2[160 - 16..], &EXPECTED_RESAMP_8_TO_16_TAIL);

        let mut res_down_1_2 = ResamplerState::default();
        res_down_1_2.init(16000, 8000).unwrap();
        let mut out_down_1_2 = [0i16; 40];
        res_down_1_2.resample(&mut out_down_1_2, &in_sig).unwrap();
        assert_eq!(&out_down_1_2[40 - 16..], &EXPECTED_RESAMP_16_TO_8_TAIL);

        let mut res_down_3_4 = ResamplerState::default();
        res_down_3_4.init(16000, 12000).unwrap();
        let mut out_down_3_4 = [0i16; 60];
        res_down_3_4.resample(&mut out_down_3_4, &in_sig).unwrap();
        assert_eq!(&out_down_3_4[60 - 16..], &EXPECTED_RESAMP_16_TO_12_TAIL);
    }
}

