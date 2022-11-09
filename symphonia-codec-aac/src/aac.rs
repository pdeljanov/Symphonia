// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// Previous Author: Kostya Shishkov <kostya.shiskov@gmail.com>
//
// This source file includes code originally written for the NihAV
// project. With the author's permission, it has been relicensed for,
// and ported to the Symphonia project.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::f32::consts;
use std::fmt;

use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Signal, SignalSpec};
use symphonia_core::codecs::{CodecDescriptor, CodecParameters, CODEC_TYPE_AAC};
use symphonia_core::codecs::{Decoder, DecoderOptions, FinalizeResult};
use symphonia_core::dsp::mdct::Imdct;
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::vlc::{Codebook, Entry16x16};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};
use symphonia_core::support_codec;
use symphonia_core::units::Duration;

use super::codebooks;
use super::common::*;
use super::window::*;

use lazy_static::lazy_static;
use log::{error, trace};

macro_rules! validate {
    ($a:expr) => {
        if !$a {
            error!("check failed at {}:{}", file!(), line!());
            return decode_error("aac: invalid data");
        }
    };
}

/// A Linear Congruential Generator (LCG) pseudo-random number generator from Numerical Recipes.
#[derive(Clone)]
struct Lcg {
    state: u32,
}

impl Lcg {
    fn new(state: u32) -> Self {
        Lcg { state }
    }

    #[inline(always)]
    fn next(&mut self) -> i32 {
        // Numerical Recipes LCG parameters.
        self.state = (self.state as u32).wrapping_mul(1664525).wrapping_add(1013904223);
        self.state as i32
    }
}

lazy_static! {
    /// Pre-computed table of y = x^(4/3).
    static ref POW43_TABLE: [f32; 8192] = {
        let mut pow43 = [0f32; 8192];
        for (i, pow43) in pow43.iter_mut().enumerate() {
            *pow43 = f32::powf(i as f32, 4.0 / 3.0);
        }
        pow43
    };
}

impl fmt::Display for M4AType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", M4A_TYPE_NAMES[*self as usize])
    }
}

struct M4AInfo {
    otype: M4AType,
    srate: u32,
    channels: usize,
    samples: usize,
    sbr_ps_info: Option<(u32, usize)>,
    sbr_present: bool,
    ps_present: bool,
}

impl M4AInfo {
    fn new() -> Self {
        Self {
            otype: M4AType::None,
            srate: 0,
            channels: 0,
            samples: 0,
            sbr_ps_info: Option::None,
            sbr_present: false,
            ps_present: false,
        }
    }

    fn read_object_type<B: ReadBitsLtr>(bs: &mut B) -> Result<M4AType> {
        let otypeidx = match bs.read_bits_leq32(5)? {
            idx if idx < 31 => idx as usize,
            31 => (bs.read_bits_leq32(6)? + 32) as usize,
            _ => unreachable!(),
        };

        if otypeidx >= M4A_TYPES.len() {
            Ok(M4AType::Unknown)
        }
        else {
            Ok(M4A_TYPES[otypeidx])
        }
    }

    fn read_sampling_frequency<B: ReadBitsLtr>(bs: &mut B) -> Result<u32> {
        match bs.read_bits_leq32(4)? {
            idx if idx < 15 => Ok(AAC_SAMPLE_RATES[idx as usize]),
            _ => {
                let srate = (0xf << 20) & bs.read_bits_leq32(20)?;
                Ok(srate)
            }
        }
    }

    fn read_channel_config<B: ReadBitsLtr>(bs: &mut B) -> Result<usize> {
        let chidx = bs.read_bits_leq32(4)? as usize;
        if chidx < AAC_CHANNELS.len() {
            Ok(AAC_CHANNELS[chidx])
        }
        else {
            Ok(chidx)
        }
    }

    fn read(&mut self, buf: &[u8]) -> Result<()> {
        let mut bs = BitReaderLtr::new(buf);

        self.otype = Self::read_object_type(&mut bs)?;
        self.srate = Self::read_sampling_frequency(&mut bs)?;

        validate!(self.srate > 0);

        self.channels = Self::read_channel_config(&mut bs)?;

        if (self.otype == M4AType::Sbr) || (self.otype == M4AType::PS) {
            let ext_srate = Self::read_sampling_frequency(&mut bs)?;
            self.otype = Self::read_object_type(&mut bs)?;

            let ext_chans = if self.otype == M4AType::ER_BSAC {
                Self::read_channel_config(&mut bs)?
            }
            else {
                0
            };

            self.sbr_ps_info = Some((ext_srate, ext_chans));
        }

        match self.otype {
            M4AType::Main
            | M4AType::Lc
            | M4AType::Ssr
            | M4AType::Scalable
            | M4AType::TwinVQ
            | M4AType::ER_AAC_LC
            | M4AType::ER_AAC_LTP
            | M4AType::ER_AAC_Scalable
            | M4AType::ER_TwinVQ
            | M4AType::ER_BSAC
            | M4AType::ER_AAC_LD => {
                // GASpecificConfig
                let short_frame = bs.read_bool()?;

                self.samples = if short_frame { 960 } else { 1024 };

                let depends_on_core = bs.read_bool()?;

                if depends_on_core {
                    let _delay = bs.read_bits_leq32(14)?;
                }

                let extension_flag = bs.read_bool()?;

                if self.channels == 0 {
                    return unsupported_error("aac: program config element");
                }

                if (self.otype == M4AType::Scalable) || (self.otype == M4AType::ER_AAC_Scalable) {
                    let _layer = bs.read_bits_leq32(3)?;
                }

                if extension_flag {
                    if self.otype == M4AType::ER_BSAC {
                        let _num_subframes = bs.read_bits_leq32(5)? as usize;
                        let _layer_length = bs.read_bits_leq32(11)?;
                    }

                    if (self.otype == M4AType::ER_AAC_LC)
                        || (self.otype == M4AType::ER_AAC_LTP)
                        || (self.otype == M4AType::ER_AAC_Scalable)
                        || (self.otype == M4AType::ER_AAC_LD)
                    {
                        let _section_data_resilience = bs.read_bool()?;
                        let _scalefactors_resilience = bs.read_bool()?;
                        let _spectral_data_resilience = bs.read_bool()?;
                    }

                    let extension_flag3 = bs.read_bool()?;

                    if extension_flag3 {
                        return unsupported_error("aac: version3 extensions");
                    }
                }
            }
            M4AType::Celp => {
                return unsupported_error("aac: CELP config");
            }
            M4AType::Hvxc => {
                return unsupported_error("aac: HVXC config");
            }
            M4AType::Ttsi => {
                return unsupported_error("aac: TTS config");
            }
            M4AType::MainSynth
            | M4AType::WavetableSynth
            | M4AType::GeneralMIDI
            | M4AType::Algorithmic => {
                return unsupported_error("aac: structured audio config");
            }
            M4AType::ER_CELP => {
                return unsupported_error("aac: ER CELP config");
            }
            M4AType::ER_HVXC => {
                return unsupported_error("aac: ER HVXC config");
            }
            M4AType::ER_HILN | M4AType::ER_Parametric => {
                return unsupported_error("aac: parametric config");
            }
            M4AType::Ssc => {
                return unsupported_error("aac: SSC config");
            }
            M4AType::MPEGSurround => {
                // bs.ignore_bits(1)?; // sacPayloadEmbedding
                return unsupported_error("aac: MPEG Surround config");
            }
            M4AType::Layer1 | M4AType::Layer2 | M4AType::Layer3 => {
                return unsupported_error("aac: MPEG Layer 1/2/3 config");
            }
            M4AType::Dst => {
                return unsupported_error("aac: DST config");
            }
            M4AType::Als => {
                // bs.ignore_bits(5)?; // fillBits
                return unsupported_error("aac: ALS config");
            }
            M4AType::Sls | M4AType::SLSNonCore => {
                return unsupported_error("aac: SLS config");
            }
            M4AType::ER_AAC_ELD => {
                return unsupported_error("aac: ELD config");
            }
            M4AType::SMRSimple | M4AType::SMRMain => {
                return unsupported_error("aac: symbolic music config");
            }
            _ => {}
        };

        match self.otype {
            M4AType::ER_AAC_LC
            | M4AType::ER_AAC_LTP
            | M4AType::ER_AAC_Scalable
            | M4AType::ER_TwinVQ
            | M4AType::ER_BSAC
            | M4AType::ER_AAC_LD
            | M4AType::ER_CELP
            | M4AType::ER_HVXC
            | M4AType::ER_HILN
            | M4AType::ER_Parametric
            | M4AType::ER_AAC_ELD => {
                let ep_config = bs.read_bits_leq32(2)?;

                if (ep_config == 2) || (ep_config == 3) {
                    return unsupported_error("aac: error protection config");
                }
                // if ep_config == 3 {
                //     let direct_mapping = bs.read_bit()?;
                //     validate!(direct_mapping);
                // }
            }
            _ => {}
        };

        if self.sbr_ps_info.is_some() && (bs.bits_left() >= 16) {
            let sync = bs.read_bits_leq32(11)?;

            if sync == 0x2B7 {
                let ext_otype = Self::read_object_type(&mut bs)?;
                if ext_otype == M4AType::Sbr {
                    self.sbr_present = bs.read_bool()?;
                    if self.sbr_present {
                        let _ext_srate = Self::read_sampling_frequency(&mut bs)?;
                        if bs.bits_left() >= 12 {
                            let sync = bs.read_bits_leq32(11)?;
                            if sync == 0x548 {
                                self.ps_present = bs.read_bool()?;
                            }
                        }
                    }
                }
                if ext_otype == M4AType::PS {
                    self.sbr_present = bs.read_bool()?;
                    if self.sbr_present {
                        let _ext_srate = Self::read_sampling_frequency(&mut bs)?;
                    }
                    let _ext_channels = bs.read_bits_leq32(4)?;
                }
            }
        }

        Ok(())
    }
}

impl fmt::Display for M4AInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MPEG 4 Audio {}, {} Hz, {} channels, {} samples per frame",
            self.otype, self.srate, self.channels, self.samples
        )
    }
}

const MAX_WINDOWS: usize = 8;
const MAX_SFBS: usize = 64;

#[derive(Clone, Copy)]
struct ICSInfo {
    window_sequence: u8,
    prev_window_sequence: u8,
    window_shape: bool,
    prev_window_shape: bool,
    scale_factor_grouping: [bool; MAX_WINDOWS],
    group_start: [usize; MAX_WINDOWS],
    window_groups: usize,
    num_windows: usize,
    max_sfb: usize,
    predictor_data: Option<LTPData>,
    long_win: bool,
}

const ONLY_LONG_SEQUENCE: u8 = 0;
const LONG_START_SEQUENCE: u8 = 1;
const EIGHT_SHORT_SEQUENCE: u8 = 2;
const LONG_STOP_SEQUENCE: u8 = 3;

impl ICSInfo {
    fn new() -> Self {
        Self {
            window_sequence: 0,
            prev_window_sequence: 0,
            window_shape: false,
            prev_window_shape: false,
            scale_factor_grouping: [false; MAX_WINDOWS],
            group_start: [0; MAX_WINDOWS],
            num_windows: 0,
            window_groups: 0,
            max_sfb: 0,
            predictor_data: None,
            long_win: true,
        }
    }

    fn decode_ics_info<B: ReadBitsLtr>(&mut self, bs: &mut B) -> Result<()> {
        self.prev_window_sequence = self.window_sequence;
        self.prev_window_shape = self.window_shape;

        if bs.read_bool()? {
            return decode_error("aac: ics reserved bit set");
        }

        self.window_sequence = bs.read_bits_leq32(2)? as u8;

        match self.prev_window_sequence {
            ONLY_LONG_SEQUENCE | LONG_STOP_SEQUENCE => {
                validate!(
                    (self.window_sequence == ONLY_LONG_SEQUENCE)
                        || (self.window_sequence == LONG_START_SEQUENCE)
                );
            }
            LONG_START_SEQUENCE | EIGHT_SHORT_SEQUENCE => {
                validate!(
                    (self.window_sequence == EIGHT_SHORT_SEQUENCE)
                        || (self.window_sequence == LONG_STOP_SEQUENCE)
                );
            }
            _ => {}
        };

        self.window_shape = bs.read_bool()?;
        self.window_groups = 1;

        if self.window_sequence == EIGHT_SHORT_SEQUENCE {
            self.long_win = false;
            self.num_windows = 8;
            self.max_sfb = bs.read_bits_leq32(4)? as usize;

            for i in 0..MAX_WINDOWS - 1 {
                self.scale_factor_grouping[i] = bs.read_bool()?;

                if !self.scale_factor_grouping[i] {
                    self.group_start[self.window_groups] = i + 1;
                    self.window_groups += 1;
                }
            }
        }
        else {
            self.long_win = true;
            self.num_windows = 1;
            self.max_sfb = bs.read_bits_leq32(6)? as usize;
            self.predictor_data = LTPData::read(bs)?;
        }
        Ok(())
    }

    fn get_group_start(&self, g: usize) -> usize {
        if g == 0 {
            0
        }
        else if g >= self.window_groups {
            if self.long_win {
                1
            }
            else {
                8
            }
        }
        else {
            self.group_start[g]
        }
    }
}

#[derive(Clone, Copy)]
struct LTPData {}

impl LTPData {
    fn read<B: ReadBitsLtr>(bs: &mut B) -> Result<Option<Self>> {
        let predictor_data_present = bs.read_bool()?;
        if !predictor_data_present {
            return Ok(None);
        }

        unsupported_error("aac: predictor data")
        /*
                if is_main {
                    let predictor_reset                         = bs.read_bit()?;
                    if predictor_reset {
                        let predictor_reset_group_number        = bs.read_bits_leq32(5)?;
                    }
                    for sfb in 0..max_sfb.min(PRED_SFB_MAX) {
                        prediction_used[sfb]                    = bs.read_bit()?;
                    }
                }
                else {
                    let ltp_data_present                        = bs.read_bit()?;
                    if ltp_data_present {
                        //ltp data
                    }
                    if common_window {
                        let ltp_data_present                    = bs.read_bit()?;
                        if ltp_data_present {
                            //ltp data
                        }
                    }
                }
                Ok(Some(Self { }))
        */
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct PulseData {
    number_pulse: usize,
    pulse_start_sfb: usize,
    pulse_offset: [u8; 4],
    pulse_amp: [u8; 4],
}

impl PulseData {
    fn read<B: ReadBitsLtr>(bs: &mut B) -> Result<Option<Self>> {
        let pulse_data_present = bs.read_bool()?;
        if !pulse_data_present {
            return Ok(None);
        }

        let number_pulse = (bs.read_bits_leq32(2)? as usize) + 1;
        let pulse_start_sfb = bs.read_bits_leq32(6)? as usize;
        let mut pulse_offset: [u8; 4] = [0; 4];
        let mut pulse_amp: [u8; 4] = [0; 4];
        for i in 0..number_pulse {
            pulse_offset[i] = bs.read_bits_leq32(5)? as u8;
            pulse_amp[i] = bs.read_bits_leq32(4)? as u8;
        }
        Ok(Some(Self { number_pulse, pulse_start_sfb, pulse_offset, pulse_amp }))
    }
}

const TNS_MAX_ORDER: usize = 20;
const TNS_MAX_LONG_BANDS: [usize; 12] = [31, 31, 34, 40, 42, 51, 46, 46, 42, 42, 42, 39];
const TNS_MAX_SHORT_BANDS: [usize; 12] = [9, 9, 10, 14, 14, 14, 14, 14, 14, 14, 14, 14];

#[derive(Clone, Copy)]
struct TNSCoeffs {
    length: usize,
    order: usize,
    direction: bool,
    coef: [f32; TNS_MAX_ORDER + 1],
}

impl TNSCoeffs {
    fn new() -> Self {
        Self { length: 0, order: 0, direction: false, coef: [0.0; TNS_MAX_ORDER + 1] }
    }

    fn read<B: ReadBitsLtr>(
        &mut self,
        bs: &mut B,
        long_win: bool,
        coef_res: bool,
        max_order: usize,
    ) -> Result<()> {
        self.length = bs.read_bits_leq32(if long_win { 6 } else { 4 })? as usize;
        self.order = bs.read_bits_leq32(if long_win { 5 } else { 3 })? as usize;

        validate!(self.order <= max_order);

        if self.order > 0 {
            self.direction = bs.read_bool()?;

            let coef_compress = bs.read_bool()?;

            // If coef_res is true, then the transmitted resolution of the filter coefficients
            // is 4 bits, otherwise it's 3 (4.6.9.2).
            let mut coef_res_bits = if coef_res { 4 } else { 3 };

            // If true, the most significant bit of the filter coefficient is not transmitted
            // (4.6.9.2).
            if coef_compress {
                coef_res_bits -= 1;
            }

            let sign_mask = 1 << (coef_res_bits - 1);
            let neg_mask = !((1 << coef_res_bits) - 1);

            // Derived from `1 << (coef_res_bits - 1)` before compression.
            let fac_base = if coef_res { 8.0 } else { 4.0 };

            let iqfac = (fac_base - 0.5) / consts::FRAC_PI_2;
            let iqfac_m = (fac_base + 0.5) / consts::FRAC_PI_2;

            let mut tmp: [f32; TNS_MAX_ORDER] = [0.0; TNS_MAX_ORDER];

            for el in tmp[..self.order].iter_mut() {
                let val = bs.read_bits_leq32(coef_res_bits)? as u8;

                // Convert to signed integer.
                let c = f32::from(if (val & sign_mask) != 0 {
                    (val | neg_mask) as i8
                }
                else {
                    val as i8
                });

                *el = (if c >= 0.0 { c / iqfac } else { c / iqfac_m }).sin();
            }

            // Generate LPC coefficients
            let mut b: [f32; TNS_MAX_ORDER + 1] = [0.0; TNS_MAX_ORDER + 1];

            for m in 1..=self.order {
                for i in 1..m {
                    b[i] = self.coef[i - 1] + tmp[m - 1] * self.coef[m - i - 1];
                }

                self.coef[..(m - 1)].copy_from_slice(&b[1..m]);
                self.coef[m - 1] = tmp[m - 1];
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct TNSData {
    n_filt: [usize; MAX_WINDOWS],
    coeffs: [[TNSCoeffs; 4]; MAX_WINDOWS],
}

impl TNSData {
    fn read<B: ReadBitsLtr>(
        bs: &mut B,
        long_win: bool,
        num_windows: usize,
        max_order: usize,
    ) -> Result<Option<Self>> {
        let tns_data_present = bs.read_bool()?;

        if !tns_data_present {
            return Ok(None);
        }

        let mut n_filt: [usize; MAX_WINDOWS] = [0; MAX_WINDOWS];
        let mut coeffs: [[TNSCoeffs; 4]; MAX_WINDOWS] = [[TNSCoeffs::new(); 4]; MAX_WINDOWS];

        for w in 0..num_windows {
            n_filt[w] = bs.read_bits_leq32(if long_win { 2 } else { 1 })? as usize;

            let coef_res = if n_filt[w] != 0 { bs.read_bool()? } else { false };

            for filt in 0..n_filt[w] {
                coeffs[w][filt].read(bs, long_win, coef_res, max_order)?;
            }
        }

        Ok(Some(Self { n_filt, coeffs }))
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct GainControlData {
    max_band: u8,
}

impl GainControlData {
    fn read<B: ReadBitsLtr>(bs: &mut B) -> Result<Option<Self>> {
        let gain_control_data_present = bs.read_bool()?;
        if !gain_control_data_present {
            return Ok(None);
        }
        unsupported_error("aac: gain control data")
        /*        self.max_band                                   = bs.read_bits_leq32(2)? as u8;
                if window_sequence == ONLY_LONG_SEQUENCE {
                    for bd in 0..max_band
        ...
                }
                Ok(Some(Self { }))*/
    }
}

const ZERO_HCB: u8 = 0;
const FIRST_PAIR_HCB: u8 = 5;
const ESC_HCB: u8 = 11;
const RESERVED_HCB: u8 = 12;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

#[derive(Clone)]
struct Ics {
    global_gain: u8,
    info: ICSInfo,
    pulse_data: Option<PulseData>,
    tns_data: Option<TNSData>,
    gain_control: Option<GainControlData>,
    sect_cb: [[u8; MAX_SFBS]; MAX_WINDOWS],
    sect_len: [[usize; MAX_SFBS]; MAX_WINDOWS],
    sfb_cb: [[u8; MAX_SFBS]; MAX_WINDOWS],
    num_sec: [usize; MAX_WINDOWS],
    scales: [[f32; MAX_SFBS]; MAX_WINDOWS],
    sbinfo: GASubbandInfo,
    coeffs: [f32; 1024],
    delay: [f32; 1024],
}

const INTENSITY_SCALE_MIN: i16 = -155;
const NOISE_SCALE_MIN: i16 = -100;

#[inline(always)]
fn get_scale(scale: i16) -> f32 {
    2.0f32.powf(0.25 * f32::from(scale - 56))
    // 2.0f32.powf(0.25 * (f32::from(scale) - 100.0 - 56.0))
}

#[inline(always)]
fn get_intensity_scale(scale: i16) -> f32 {
    0.5f32.powf(0.25 * f32::from(scale))
}

impl Ics {
    fn new(sbinfo: GASubbandInfo) -> Self {
        Self {
            global_gain: 0,
            info: ICSInfo::new(),
            pulse_data: None,
            tns_data: None,
            gain_control: None,
            sect_cb: [[0; MAX_SFBS]; MAX_WINDOWS],
            sect_len: [[0; MAX_SFBS]; MAX_WINDOWS],
            sfb_cb: [[0; MAX_SFBS]; MAX_WINDOWS],
            scales: [[0.0; MAX_SFBS]; MAX_WINDOWS],
            num_sec: [0; MAX_WINDOWS],
            sbinfo,
            coeffs: [0.0; 1024],
            delay: [0.0; 1024],
        }
    }

    fn reset(&mut self) {
        self.info = ICSInfo::new();
        self.delay = [0.0; 1024];
    }

    fn decode_section_data<B: ReadBitsLtr>(&mut self, bs: &mut B) -> Result<()> {
        let sect_bits = if self.info.long_win { 5 } else { 3 };
        let sect_esc_val = (1 << sect_bits) - 1;

        for g in 0..self.info.window_groups {
            let mut k = 0;
            let mut l = 0;

            while k < self.info.max_sfb {
                self.sect_cb[g][l] = bs.read_bits_leq32(4)? as u8;
                self.sect_len[g][l] = 0;

                if self.sect_cb[g][l] == RESERVED_HCB {
                    return decode_error("aac: invalid band type");
                }

                loop {
                    let sect_len_incr = bs.read_bits_leq32(sect_bits)? as usize;

                    self.sect_len[g][l] += sect_len_incr;

                    if sect_len_incr < sect_esc_val {
                        break;
                    }
                }

                validate!(k + self.sect_len[g][l] <= self.info.max_sfb);

                for sfb in k..k + self.sect_len[g][l] {
                    self.sfb_cb[g][sfb] = self.sect_cb[g][l];
                }

                k += self.sect_len[g][l];
                l += 1;
            }

            self.num_sec[g] = l;
        }
        Ok(())
    }

    #[inline(always)]
    fn is_zero(&self, g: usize, sfb: usize) -> bool {
        self.sfb_cb[g][sfb] == ZERO_HCB
    }

    #[inline(always)]
    fn is_intensity(&self, g: usize, sfb: usize) -> bool {
        (self.sfb_cb[g][sfb] == INTENSITY_HCB) || (self.sfb_cb[g][sfb] == INTENSITY_HCB2)
    }

    #[inline(always)]
    fn is_noise(&self, g: usize, sfb: usize) -> bool {
        self.sfb_cb[g][sfb] == NOISE_HCB
    }

    #[inline(always)]
    fn get_intensity_dir(&self, g: usize, sfb: usize) -> bool {
        self.sfb_cb[g][sfb] == INTENSITY_HCB
    }

    fn decode_scale_factor_data<B: ReadBitsLtr>(&mut self, bs: &mut B) -> Result<()> {
        let mut noise_pcm_flag = true;
        let mut scf_intensity = 0i16;
        let mut scf_noise = i16::from(self.global_gain) - 90;
        let mut scf_normal = i16::from(self.global_gain);

        let scf_table = &codebooks::SCF_CODEBOOK;

        for g in 0..self.info.window_groups {
            for sfb in 0..self.info.max_sfb {
                self.scales[g][sfb] = if self.is_zero(g, sfb) {
                    0.0
                }
                else if self.is_intensity(g, sfb) {
                    scf_intensity += i16::from(bs.read_codebook(scf_table)?.0) - 60;

                    validate!(
                        (scf_intensity >= INTENSITY_SCALE_MIN)
                            && (scf_intensity < INTENSITY_SCALE_MIN + 256)
                    );

                    get_intensity_scale(scf_intensity)
                }
                else if self.is_noise(g, sfb) {
                    if noise_pcm_flag {
                        noise_pcm_flag = false;
                        scf_noise += (bs.read_bits_leq32(9)? as i16) - 256;
                    }
                    else {
                        scf_noise += i16::from(bs.read_codebook(scf_table)?.0) - 60;
                    }

                    validate!(
                        (scf_noise >= NOISE_SCALE_MIN) && (scf_noise < NOISE_SCALE_MIN + 256)
                    );

                    get_scale(scf_noise)
                }
                else {
                    scf_normal += i16::from(bs.read_codebook(scf_table)?.0) - 60;
                    validate!((scf_normal >= 0) && (scf_normal < 256));

                    get_scale(scf_normal - 100)
                }
            }
        }
        Ok(())
    }

    fn get_band_start(&self, swb: usize) -> usize {
        if self.info.long_win {
            self.sbinfo.long_bands[swb]
        }
        else {
            self.sbinfo.short_bands[swb]
        }
    }

    fn get_num_bands(&self) -> usize {
        if self.info.long_win {
            self.sbinfo.long_bands.len() - 1
        }
        else {
            self.sbinfo.short_bands.len() - 1
        }
    }

    fn decode_spectrum<B: ReadBitsLtr>(&mut self, bs: &mut B, lcg: &mut Lcg) -> Result<()> {
        // Zero all spectral coefficients.
        self.coeffs = [0.0; 1024];
        for g in 0..self.info.window_groups {
            let cur_w = self.info.get_group_start(g);
            let next_w = self.info.get_group_start(g + 1);
            for sfb in 0..self.info.max_sfb {
                let start = self.get_band_start(sfb);
                let end = self.get_band_start(sfb + 1);
                let cb_idx = self.sfb_cb[g][sfb];
                for w in cur_w..next_w {
                    let dst = &mut self.coeffs[start + w * 128..end + w * 128];

                    let scale = self.scales[g][sfb];

                    match cb_idx {
                        ZERO_HCB => (),
                        NOISE_HCB => decode_noise(lcg, scale, dst),
                        INTENSITY_HCB | INTENSITY_HCB2 => (),
                        _ => {
                            let unsigned = AAC_UNSIGNED_CODEBOOK[(cb_idx - 1) as usize];

                            let cb = &codebooks::SPECTRUM_CODEBOOKS[(cb_idx - 1) as usize];

                            if cb_idx < FIRST_PAIR_HCB {
                                decode_quads(bs, cb, unsigned, scale, dst)?;
                            }
                            else {
                                decode_pairs(
                                    bs,
                                    cb,
                                    unsigned,
                                    cb_idx == ESC_HCB,
                                    AAC_CODEBOOK_MODULO[(cb_idx - FIRST_PAIR_HCB) as usize],
                                    scale,
                                    dst,
                                )?;
                            }
                        }
                    };
                }
            }
        }
        Ok(())
    }

    fn place_pulses(&mut self) {
        if let Some(ref pdata) = self.pulse_data {
            if pdata.pulse_start_sfb >= self.sbinfo.long_bands.len() - 1 {
                return;
            }

            let mut k = self.get_band_start(pdata.pulse_start_sfb);

            let mut band = pdata.pulse_start_sfb;

            for pno in 0..pdata.number_pulse {
                k += pdata.pulse_offset[pno] as usize;

                if k >= 1024 {
                    return;
                }

                while self.get_band_start(band + 1) <= k {
                    band += 1;
                }

                let scale = self.scales[0][band];
                let mut base = self.coeffs[k];

                if base != 0.0 {
                    base = requant(self.coeffs[k], scale);
                }

                if base > 0.0 {
                    base += f32::from(pdata.pulse_amp[pno]);
                }
                else {
                    base -= f32::from(pdata.pulse_amp[pno]);
                }
                self.coeffs[k] = iquant(base) * scale;
            }
        }
    }

    fn decode_ics<B: ReadBitsLtr>(
        &mut self,
        bs: &mut B,
        lcg: &mut Lcg,
        m4atype: M4AType,
        common_window: bool,
    ) -> Result<()> {
        self.global_gain = bs.read_bits_leq32(8)? as u8;

        if !common_window {
            self.info.decode_ics_info(bs)?;
        }

        self.decode_section_data(bs)?;

        self.decode_scale_factor_data(bs)?;

        self.pulse_data = PulseData::read(bs)?;
        validate!(self.pulse_data.is_none() || self.info.long_win);

        // Table 4.156
        let tns_max_order = if !self.info.long_win {
            7
        }
        else if m4atype == M4AType::Lc {
            12
        }
        else {
            TNS_MAX_ORDER
        };

        self.tns_data =
            TNSData::read(bs, self.info.long_win, self.info.num_windows, tns_max_order)?;

        match m4atype {
            M4AType::Ssr => self.gain_control = GainControlData::read(bs)?,
            _ => {
                let gain_control_data_present = bs.read_bool()?;
                validate!(!gain_control_data_present);
            }
        }

        self.decode_spectrum(bs, lcg)?;
        Ok(())
    }

    fn synth_channel(&mut self, dsp: &mut Dsp, srate_idx: usize, dst: &mut [f32]) {
        self.place_pulses();

        if let Some(ref tns_data) = self.tns_data {
            let tns_max_bands = (if self.info.long_win {
                TNS_MAX_LONG_BANDS[srate_idx]
            }
            else {
                TNS_MAX_SHORT_BANDS[srate_idx]
            })
            .min(self.info.max_sfb);

            for w in 0..self.info.num_windows {
                let mut bottom = self.get_num_bands();

                for f in 0..tns_data.n_filt[w] {
                    let top = bottom;

                    bottom = if top > tns_data.coeffs[w][f].length {
                        top - tns_data.coeffs[w][f].length
                    }
                    else {
                        0
                    };

                    let order = tns_data.coeffs[w][f].order;

                    if order == 0 {
                        continue;
                    }

                    let start = w * 128 + self.get_band_start(bottom.min(tns_max_bands));
                    let end = w * 128 + self.get_band_start(top.min(tns_max_bands));
                    let lpc = &tns_data.coeffs[w][f].coef;

                    if !tns_data.coeffs[w][f].direction {
                        for (m, i) in (start..end).enumerate() {
                            for j in 0..order.min(m) {
                                self.coeffs[i] -= self.coeffs[i - j - 1] * lpc[j];
                            }
                        }
                    }
                    else {
                        for (m, i) in (start..end).rev().enumerate() {
                            for j in 0..order.min(m) {
                                self.coeffs[i] -= self.coeffs[i + j + 1] * lpc[j];
                            }
                        }
                    }
                }
            }
        }

        dsp.synth(
            &self.coeffs,
            &mut self.delay,
            self.info.window_sequence,
            self.info.window_shape,
            self.info.prev_window_shape,
            dst,
        );
    }
}

#[inline(always)]
fn iquant(val: f32) -> f32 {
    if val < 0.0 {
        -((-val).powf(4.0 / 3.0))
    }
    else {
        val.powf(4.0 / 3.0)
    }
}

#[inline(always)]
fn requant(val: f32, scale: f32) -> f32 {
    if scale == 0.0 {
        return 0.0;
    }
    let bval = val / scale;
    if bval >= 0.0 {
        val.powf(3.0 / 4.0)
    }
    else {
        -((-val).powf(3.0 / 4.0))
    }
}

/// Perceptual noise substitution decode step. Section 4.6.13.3.
fn decode_noise(lcg: &mut Lcg, sf: f32, dst: &mut [f32]) {
    let mut energy = 0.0;

    for spec in dst.iter_mut() {
        // The random number generator outputs i32, but the largest signed
        // integer that can convert to f32 is i16.
        *spec = f32::from((lcg.next() >> 16) as i16);
        energy += *spec * *spec;
    }

    let scale = sf / energy.sqrt();

    for spec in dst.iter_mut() {
        *spec *= scale;
    }
}

fn decode_quads<B: ReadBitsLtr>(
    bs: &mut B,
    cb: &Codebook<Entry16x16>,
    unsigned: bool,
    scale: f32,
    dst: &mut [f32],
) -> Result<()> {
    let pow43_table: &[f32; 8192] = &POW43_TABLE;

    for out in dst.chunks_mut(4) {
        let cw = bs.read_codebook(cb)?.0 as usize;
        if unsigned {
            for (out, &quad) in out.iter_mut().zip(&AAC_QUADS[cw]) {
                if quad != 0 {
                    *out = if bs.read_bool()? {
                        -pow43_table[quad as usize] * scale
                    }
                    else {
                        pow43_table[quad as usize] * scale
                    }
                }
            }
        }
        else {
            for (out, &quad) in out.iter_mut().zip(&AAC_QUADS[cw]) {
                let val = quad - 1;

                *out = if val < 0 {
                    -pow43_table[-val as usize] * scale
                }
                else {
                    pow43_table[val as usize] * scale
                }
            }
        }
    }
    Ok(())
}

fn decode_pairs<B: ReadBitsLtr>(
    bs: &mut B,
    cb: &Codebook<Entry16x16>,
    unsigned: bool,
    escape: bool,
    modulo: u16,
    scale: f32,
    dst: &mut [f32],
) -> Result<()> {
    let pow43_table: &[f32; 8192] = &POW43_TABLE;

    for out in dst.chunks_mut(2) {
        let cw = bs.read_codebook(cb)?.0;

        let mut x = (cw / modulo) as i16;
        let mut y = (cw % modulo) as i16;

        if unsigned {
            if x != 0 && bs.read_bool()? {
                x = -x;
            }
            if y != 0 && bs.read_bool()? {
                y = -y;
            }
        }
        else {
            x -= (modulo >> 1) as i16;
            y -= (modulo >> 1) as i16;
        }

        if escape {
            if (x == 16) || (x == -16) {
                x = read_escape(bs, x.is_positive())?;
            }
            if (y == 16) || (y == -16) {
                y = read_escape(bs, y.is_positive())?;
            }
        }

        out[0] = if x < 0 { -pow43_table[-x as usize] } else { pow43_table[x as usize] } * scale;
        out[1] = if y < 0 { -pow43_table[-y as usize] } else { pow43_table[y as usize] } * scale;
    }
    Ok(())
}

fn read_escape<B: ReadBitsLtr>(bs: &mut B, is_pos: bool) -> Result<i16> {
    let n = bs.read_unary_ones()?;

    validate!(n < 9);

    // The escape word is added to 2^(n + 4) to yield the unsigned value.
    let word = (1 << (n + 4)) + bs.read_bits_leq32(n + 4)? as i16;

    if is_pos {
        Ok(word)
    }
    else {
        Ok(-word)
    }
}

#[derive(Clone)]
struct ChannelPair {
    is_pair: bool,
    channel: usize,
    ms_mask_present: u8,
    ms_used: [[bool; MAX_SFBS]; MAX_WINDOWS],
    ics0: Ics,
    ics1: Ics,
    lcg: Lcg,
}

impl ChannelPair {
    fn new(is_pair: bool, channel: usize, sbinfo: GASubbandInfo) -> Self {
        Self {
            is_pair,
            channel,
            ms_mask_present: 0,
            ms_used: [[false; MAX_SFBS]; MAX_WINDOWS],
            ics0: Ics::new(sbinfo),
            ics1: Ics::new(sbinfo),
            lcg: Lcg::new(0x1f2e3d4c), // Use the same seed as ffmpeg for symphonia-check.
        }
    }

    fn reset(&mut self) {
        self.ics0.reset();
        self.ics1.reset();
    }

    fn decode_ga_sce<B: ReadBitsLtr>(&mut self, bs: &mut B, m4atype: M4AType) -> Result<()> {
        self.ics0.decode_ics(bs, &mut self.lcg, m4atype, false)?;
        Ok(())
    }

    fn decode_ga_cpe<B: ReadBitsLtr>(&mut self, bs: &mut B, m4atype: M4AType) -> Result<()> {
        let common_window = bs.read_bool()?;

        if common_window {
            self.ics0.info.decode_ics_info(bs)?;

            // Mid-side stereo mask decoding.
            self.ms_mask_present = bs.read_bits_leq32(2)? as u8;

            match self.ms_mask_present {
                0 | 2 => (),
                1 => {
                    for g in 0..self.ics0.info.window_groups {
                        for sfb in 0..self.ics0.info.max_sfb {
                            self.ms_used[g][sfb] = bs.read_bool()?;
                        }
                    }
                }
                3 => return decode_error("aac: invalid mid-side mask"),
                _ => unreachable!(),
            }

            self.ics1.info = self.ics0.info;
        }

        self.ics0.decode_ics(bs, &mut self.lcg, m4atype, common_window)?;
        self.ics1.decode_ics(bs, &mut self.lcg, m4atype, common_window)?;

        // Joint-stereo decoding
        if common_window && self.ms_mask_present != 0 {
            let mut g = 0;

            for w in 0..self.ics0.info.num_windows {
                if w > 0 && !self.ics0.info.scale_factor_grouping[w - 1] {
                    g += 1;
                }

                for sfb in 0..self.ics0.info.max_sfb {
                    let start = w * 128 + self.ics0.get_band_start(sfb);
                    let end = w * 128 + self.ics0.get_band_start(sfb + 1);

                    if self.ics1.is_intensity(g, sfb) {
                        // Intensity stereo
                        // Section 4.6.8.2.3
                        let invert = self.ms_mask_present == 1 && self.ms_used[g][sfb];
                        let dir = if self.ics1.get_intensity_dir(g, sfb) { 1.0 } else { -1.0 };
                        let factor = if invert { -1.0 } else { 1.0 };

                        let scale = dir * factor * self.ics1.scales[g][sfb];

                        let left = &self.ics0.coeffs[start..end];
                        let right = &mut self.ics1.coeffs[start..end];

                        for (l, r) in left.iter().zip(right) {
                            *r = scale * l;
                        }
                    }
                    else if self.ics0.is_noise(g, sfb) || self.ics1.is_noise(g, sfb) {
                        // Perceptual noise substitution, do not do joint-stereo decoding.
                        // Section 4.6.13.3
                    }
                    else if self.ms_mask_present == 2 || self.ms_used[g][sfb] {
                        // Mid-side stereo.
                        let mid = &mut self.ics0.coeffs[start..end];
                        let side = &mut self.ics1.coeffs[start..end];

                        for (m, s) in mid.iter_mut().zip(side) {
                            let tmp = *m - *s;
                            *m += *s;
                            *s = tmp;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn synth_audio(&mut self, dsp: &mut Dsp, abuf: &mut AudioBuffer<f32>, srate_idx: usize) {
        self.ics0.synth_channel(dsp, srate_idx, abuf.chan_mut(self.channel));

        if self.is_pair {
            self.ics1.synth_channel(dsp, srate_idx, abuf.chan_mut(self.channel + 1));
        }
    }
}

struct Dsp {
    kbd_long_win: [f32; 1024],
    kbd_short_win: [f32; 128],
    sine_long_win: [f32; 1024],
    sine_short_win: [f32; 128],
    imdct_long: Imdct,
    imdct_short: Imdct,
    tmp: [f32; 2048],
    ew_buf: [f32; 1152],
}

const SHORT_WIN_POINT0: usize = 512 - 64;
const SHORT_WIN_POINT1: usize = 512 + 64;

impl Dsp {
    fn new() -> Self {
        let mut kbd_long_win: [f32; 1024] = [0.0; 1024];
        let mut kbd_short_win: [f32; 128] = [0.0; 128];
        generate_window(WindowType::KaiserBessel(4.0), 1.0, 1024, true, &mut kbd_long_win);
        generate_window(WindowType::KaiserBessel(6.0), 1.0, 128, true, &mut kbd_short_win);
        let mut sine_long_win: [f32; 1024] = [0.0; 1024];
        let mut sine_short_win: [f32; 128] = [0.0; 128];
        generate_window(WindowType::Sine, 1.0, 1024, true, &mut sine_long_win);
        generate_window(WindowType::Sine, 1.0, 128, true, &mut sine_short_win);

        Self {
            kbd_long_win,
            kbd_short_win,
            sine_long_win,
            sine_short_win,
            imdct_long: Imdct::new_scaled(1024, 1.0 / 2048.0),
            imdct_short: Imdct::new_scaled(128, 1.0 / 256.0),
            tmp: [0.0; 2048],
            ew_buf: [0.0; 1152],
        }
    }

    #[allow(clippy::cognitive_complexity)]
    fn synth(
        &mut self,
        coeffs: &[f32; 1024],
        delay: &mut [f32; 1024],
        seq: u8,
        window_shape: bool,
        prev_window_shape: bool,
        dst: &mut [f32],
    ) {
        let (long_win, short_win) = match window_shape {
            true => (&self.kbd_long_win, &self.kbd_short_win),
            false => (&self.sine_long_win, &self.sine_short_win),
        };

        let (prev_long_win, prev_short_win) = match prev_window_shape {
            true => (&self.kbd_long_win, &self.kbd_short_win),
            false => (&self.sine_long_win, &self.sine_short_win),
        };

        // Zero the output buffer.
        self.tmp = [0.0; 2048];

        // Inverse MDCT
        if seq != EIGHT_SHORT_SEQUENCE {
            self.imdct_long.imdct(coeffs, &mut self.tmp);
        }
        else {
            for (ain, aout) in coeffs.chunks(128).zip(self.tmp.chunks_mut(256)) {
                self.imdct_short.imdct(ain, aout);
            }

            self.ew_buf = [0.0; 1152];

            for (w, src) in self.tmp.chunks(256).enumerate() {
                if w > 0 {
                    for i in 0..128 {
                        self.ew_buf[w * 128 + i + 0] += src[i + 0] * short_win[i];
                        self.ew_buf[w * 128 + i + 128] += src[i + 128] * short_win[127 - i];
                    }
                }
                else {
                    for i in 0..128 {
                        self.ew_buf[i + 0] = src[i + 0] * prev_short_win[i];
                        self.ew_buf[i + 128] = src[i + 128] * short_win[127 - i];
                    }
                }
            }
        }

        // output new data
        match seq {
            ONLY_LONG_SEQUENCE | LONG_START_SEQUENCE => {
                for i in 0..1024 {
                    dst[i] = delay[i] + (self.tmp[i] * prev_long_win[i]);
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                dst[..SHORT_WIN_POINT0].copy_from_slice(&delay[..SHORT_WIN_POINT0]);

                for i in SHORT_WIN_POINT0..1024 {
                    dst[i] = delay[i] + self.ew_buf[i - SHORT_WIN_POINT0];
                }
            }
            LONG_STOP_SEQUENCE => {
                dst[..SHORT_WIN_POINT0].copy_from_slice(&delay[..SHORT_WIN_POINT0]);

                for i in SHORT_WIN_POINT0..SHORT_WIN_POINT1 {
                    dst[i] = delay[i] + self.tmp[i] * prev_short_win[i - SHORT_WIN_POINT0];
                }
                for i in SHORT_WIN_POINT1..1024 {
                    dst[i] = delay[i] + self.tmp[i];
                }
            }
            _ => unreachable!(),
        };

        // save delay
        match seq {
            ONLY_LONG_SEQUENCE | LONG_STOP_SEQUENCE => {
                for i in 0..1024 {
                    delay[i] = self.tmp[i + 1024] * long_win[1023 - i];
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                for i in 0..SHORT_WIN_POINT1 {
                    // last part is already windowed
                    delay[i] = self.ew_buf[i + 512 + 64];
                }
                for i in SHORT_WIN_POINT1..1024 {
                    delay[i] = 0.0;
                }
            }
            LONG_START_SEQUENCE => {
                delay[..SHORT_WIN_POINT0]
                    .copy_from_slice(&self.tmp[1024..(SHORT_WIN_POINT0 + 1024)]);

                for i in SHORT_WIN_POINT0..SHORT_WIN_POINT1 {
                    delay[i] = self.tmp[i + 1024] * short_win[127 - (i - SHORT_WIN_POINT0)];
                }
                for i in SHORT_WIN_POINT1..1024 {
                    delay[i] = 0.0;
                }
            }
            _ => unreachable!(),
        };
    }
}

/// Advanced Audio Coding (AAC) decoder.
///
/// Implements a decoder for Advanced Audio Decoding Low-Complexity (AAC-LC) as defined in
/// ISO/IEC 13818-7 and ISO/IEC 14496-3.
pub struct AacDecoder {
    // info: NACodecInfoRef,
    m4ainfo: M4AInfo,
    pairs: Vec<ChannelPair>,
    dsp: Dsp,
    sbinfo: GASubbandInfo,
    params: CodecParameters,
    buf: AudioBuffer<f32>,
}

impl AacDecoder {
    fn set_pair(&mut self, pair_no: usize, channel: usize, pair: bool) -> Result<()> {
        if self.pairs.len() <= pair_no {
            self.pairs.push(ChannelPair::new(pair, channel, self.sbinfo));
        }
        else {
            validate!(self.pairs[pair_no].channel == channel);
            validate!(self.pairs[pair_no].is_pair == pair);
        }
        validate!(if pair { channel + 1 } else { channel } < self.m4ainfo.channels);
        Ok(())
    }

    fn decode_ga<B: ReadBitsLtr + FiniteBitStream>(&mut self, bs: &mut B) -> Result<()> {
        let mut cur_pair = 0;
        let mut cur_ch = 0;
        while bs.bits_left() > 3 {
            let id = bs.read_bits_leq32(3)?;

            match id {
                0 => {
                    // ID_SCE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, false)?;
                    self.pairs[cur_pair].decode_ga_sce(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 1;
                }
                1 => {
                    // ID_CPE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, true)?;
                    self.pairs[cur_pair].decode_ga_cpe(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 2;
                }
                2 => {
                    // ID_CCE
                    return unsupported_error("aac: coupling channel element");
                }
                3 => {
                    // ID_LFE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, false)?;
                    self.pairs[cur_pair].decode_ga_sce(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 1;
                }
                4 => {
                    // ID_DSE
                    let _id = bs.read_bits_leq32(4)?;
                    let align = bs.read_bool()?;
                    let mut count = bs.read_bits_leq32(8)? as u32;
                    if count == 255 {
                        count += bs.read_bits_leq32(8)? as u32;
                    }
                    if align {
                        bs.realign(); // ????
                    }
                    bs.ignore_bits(count * 8)?; // no SBR payload or such
                }
                5 => {
                    // ID_PCE
                    return unsupported_error("aac: program config");
                }
                6 => {
                    // ID_FIL
                    let mut count = bs.read_bits_leq32(4)? as usize;
                    if count == 15 {
                        count += bs.read_bits_leq32(8)? as usize;
                        count -= 1;
                    }
                    for _ in 0..count {
                        // ext payload
                        bs.ignore_bits(8)?;
                    }
                }
                7 => {
                    // ID_TERM
                    break;
                }
                _ => unreachable!(),
            };
        }
        let srate_idx = GASubbandInfo::find_idx(self.m4ainfo.srate);
        for pair in 0..cur_pair {
            self.pairs[pair].synth_audio(&mut self.dsp, &mut self.buf, srate_idx);
        }
        Ok(())
    }

    // fn flush(&mut self) {
    //     for pair in self.pairs.iter_mut() {
    //         pair.ics[0].delay = [0.0; 1024];
    //         pair.ics[1].delay = [0.0; 1024];
    //     }
    // }

    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        // Clear the audio output buffer.
        self.buf.clear();
        self.buf.render_reserved(None);

        let mut bs = BitReaderLtr::new(packet.buf());

        // Choose decode step based on the object type.
        match self.m4ainfo.otype {
            M4AType::Lc => self.decode_ga(&mut bs)?,
            _ => return unsupported_error("aac: object type"),
        }

        Ok(())
    }
}

impl Decoder for AacDecoder {
    fn try_new(params: &CodecParameters, _: &DecoderOptions) -> Result<Self> {
        // This decoder only supports AAC.
        if params.codec != CODEC_TYPE_AAC {
            return unsupported_error("aac: invalid codec type");
        }

        let mut m4ainfo = M4AInfo::new();

        // If extra data present, parse the audio specific config
        if let Some(extra_data_buf) = &params.extra_data {
            validate!(extra_data_buf.len() >= 2);
            m4ainfo.read(extra_data_buf)?;
        }
        else {
            validate!(params.sample_rate.is_some());
            validate!(params.channels.is_some());

            // Otherwise, assume there is no ASC and use the codec parameters for ADTS.
            m4ainfo.srate = params.sample_rate.unwrap();
            m4ainfo.otype = M4AType::Lc;
            m4ainfo.samples = 1024;
            m4ainfo.channels = params.channels.unwrap().count();
        }

        //print!("edata:"); for s in edata.iter() { print!(" {:02X}", *s);}println!("");

        trace!("{}", m4ainfo);

        if (m4ainfo.otype != M4AType::Lc) || (m4ainfo.channels > 2) || (m4ainfo.samples != 1024) {
            return unsupported_error("aac: aac too complex");
        }

        let spec = SignalSpec::new(m4ainfo.srate, map_channels(m4ainfo.channels as u32).unwrap());

        let duration = m4ainfo.samples as Duration;
        let srate = m4ainfo.srate;

        Ok(AacDecoder {
            m4ainfo,
            pairs: Vec::new(),
            dsp: Dsp::new(),
            sbinfo: GASubbandInfo::find(srate),
            params: params.clone(),
            buf: AudioBuffer::new(duration, spec),
        })
    }

    fn reset(&mut self) {
        for pair in self.pairs.iter_mut() {
            pair.reset();
        }
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_AAC, "aac", "Advanced Audio Coding")]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        }
        else {
            Ok(self.buf.as_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}

const AAC_UNSIGNED_CODEBOOK: [bool; 11] =
    [false, false, true, true, false, false, true, true, true, true, true];

const AAC_CODEBOOK_MODULO: [u16; 7] = [9, 9, 8, 8, 13, 13, 17];

const AAC_QUADS: [[i8; 4]; 81] = [
    [0, 0, 0, 0],
    [0, 0, 0, 1],
    [0, 0, 0, 2],
    [0, 0, 1, 0],
    [0, 0, 1, 1],
    [0, 0, 1, 2],
    [0, 0, 2, 0],
    [0, 0, 2, 1],
    [0, 0, 2, 2],
    [0, 1, 0, 0],
    [0, 1, 0, 1],
    [0, 1, 0, 2],
    [0, 1, 1, 0],
    [0, 1, 1, 1],
    [0, 1, 1, 2],
    [0, 1, 2, 0],
    [0, 1, 2, 1],
    [0, 1, 2, 2],
    [0, 2, 0, 0],
    [0, 2, 0, 1],
    [0, 2, 0, 2],
    [0, 2, 1, 0],
    [0, 2, 1, 1],
    [0, 2, 1, 2],
    [0, 2, 2, 0],
    [0, 2, 2, 1],
    [0, 2, 2, 2],
    [1, 0, 0, 0],
    [1, 0, 0, 1],
    [1, 0, 0, 2],
    [1, 0, 1, 0],
    [1, 0, 1, 1],
    [1, 0, 1, 2],
    [1, 0, 2, 0],
    [1, 0, 2, 1],
    [1, 0, 2, 2],
    [1, 1, 0, 0],
    [1, 1, 0, 1],
    [1, 1, 0, 2],
    [1, 1, 1, 0],
    [1, 1, 1, 1],
    [1, 1, 1, 2],
    [1, 1, 2, 0],
    [1, 1, 2, 1],
    [1, 1, 2, 2],
    [1, 2, 0, 0],
    [1, 2, 0, 1],
    [1, 2, 0, 2],
    [1, 2, 1, 0],
    [1, 2, 1, 1],
    [1, 2, 1, 2],
    [1, 2, 2, 0],
    [1, 2, 2, 1],
    [1, 2, 2, 2],
    [2, 0, 0, 0],
    [2, 0, 0, 1],
    [2, 0, 0, 2],
    [2, 0, 1, 0],
    [2, 0, 1, 1],
    [2, 0, 1, 2],
    [2, 0, 2, 0],
    [2, 0, 2, 1],
    [2, 0, 2, 2],
    [2, 1, 0, 0],
    [2, 1, 0, 1],
    [2, 1, 0, 2],
    [2, 1, 1, 0],
    [2, 1, 1, 1],
    [2, 1, 1, 2],
    [2, 1, 2, 0],
    [2, 1, 2, 1],
    [2, 1, 2, 2],
    [2, 2, 0, 0],
    [2, 2, 0, 1],
    [2, 2, 0, 2],
    [2, 2, 1, 0],
    [2, 2, 1, 1],
    [2, 2, 1, 2],
    [2, 2, 2, 0],
    [2, 2, 2, 1],
    [2, 2, 2, 2],
];

const SWB_OFFSET_48K_LONG: [usize; 49 + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 1024,
];

const SWB_OFFSET_48K_SHORT: [usize; 14 + 1] =
    [0, 4, 8, 12, 16, 20, 28, 36, 44, 56, 68, 80, 96, 112, 128];

const SWB_OFFSET_32K_LONG: [usize; 51 + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 960, 992, 1024,
];

const SWB_OFFSET_8K_LONG: [usize; 40 + 1] = [
    0, 12, 24, 36, 48, 60, 72, 84, 96, 108, 120, 132, 144, 156, 172, 188, 204, 220, 236, 252, 268,
    288, 308, 328, 348, 372, 396, 420, 448, 476, 508, 544, 580, 620, 664, 712, 764, 820, 880, 944,
    1024,
];

const SWB_OFFSET_8K_SHORT: [usize; 15 + 1] =
    [0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 60, 72, 88, 108, 128];

const SWB_OFFSET_16K_LONG: [usize; 43 + 1] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 100, 112, 124, 136, 148, 160, 172, 184, 196, 212,
    228, 244, 260, 280, 300, 320, 344, 368, 396, 424, 456, 492, 532, 572, 616, 664, 716, 772, 832,
    896, 960, 1024,
];

const SWB_OFFSET_16K_SHORT: [usize; 15 + 1] =
    [0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 60, 72, 88, 108, 128];

const SWB_OFFSET_24K_LONG: [usize; 47 + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 136,
    148, 160, 172, 188, 204, 220, 240, 260, 284, 308, 336, 364, 396, 432, 468, 508, 552, 600, 652,
    704, 768, 832, 896, 960, 1024,
];

const SWB_OFFSET_24K_SHORT: [usize; 15 + 1] =
    [0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 64, 76, 92, 108, 128];

const SWB_OFFSET_64K_LONG: [usize; 47 + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 100, 112, 124, 140,
    156, 172, 192, 216, 240, 268, 304, 344, 384, 424, 464, 504, 544, 584, 624, 664, 704, 744, 784,
    824, 864, 904, 944, 984, 1024,
];

const SWB_OFFSET_64K_SHORT: [usize; 12 + 1] = [0, 4, 8, 12, 16, 20, 24, 32, 40, 48, 64, 92, 128];

const SWB_OFFSET_96K_LONG: [usize; 41 + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132,
    144, 156, 172, 188, 212, 240, 276, 320, 384, 448, 512, 576, 640, 704, 768, 832, 896, 960, 1024,
];

#[derive(Clone, Copy)]
struct GASubbandInfo {
    min_srate: u32,
    long_bands: &'static [usize],
    short_bands: &'static [usize],
}

impl GASubbandInfo {
    fn find(srate: u32) -> GASubbandInfo {
        for sbi in AAC_SUBBAND_INFO.iter() {
            if srate >= sbi.min_srate {
                return *sbi;
            }
        }
        unreachable!()
    }
    fn find_idx(srate: u32) -> usize {
        for (i, sbi) in AAC_SUBBAND_INFO.iter().enumerate() {
            if srate >= sbi.min_srate {
                return i;
            }
        }
        unreachable!()
    }
}

const AAC_SUBBAND_INFO: [GASubbandInfo; 12] = [
    GASubbandInfo {
        min_srate: 92017,
        long_bands: &SWB_OFFSET_96K_LONG,
        short_bands: &SWB_OFFSET_64K_SHORT,
    }, //96K
    GASubbandInfo {
        min_srate: 75132,
        long_bands: &SWB_OFFSET_96K_LONG,
        short_bands: &SWB_OFFSET_64K_SHORT,
    }, //88.2K
    GASubbandInfo {
        min_srate: 55426,
        long_bands: &SWB_OFFSET_64K_LONG,
        short_bands: &SWB_OFFSET_64K_SHORT,
    }, //64K
    GASubbandInfo {
        min_srate: 46009,
        long_bands: &SWB_OFFSET_48K_LONG,
        short_bands: &SWB_OFFSET_48K_SHORT,
    }, //48K
    GASubbandInfo {
        min_srate: 37566,
        long_bands: &SWB_OFFSET_48K_LONG,
        short_bands: &SWB_OFFSET_48K_SHORT,
    }, //44.1K
    GASubbandInfo {
        min_srate: 27713,
        long_bands: &SWB_OFFSET_32K_LONG,
        short_bands: &SWB_OFFSET_48K_SHORT,
    }, //32K
    GASubbandInfo {
        min_srate: 23004,
        long_bands: &SWB_OFFSET_24K_LONG,
        short_bands: &SWB_OFFSET_24K_SHORT,
    }, //24K
    GASubbandInfo {
        min_srate: 18783,
        long_bands: &SWB_OFFSET_24K_LONG,
        short_bands: &SWB_OFFSET_24K_SHORT,
    }, //22.05K
    GASubbandInfo {
        min_srate: 13856,
        long_bands: &SWB_OFFSET_16K_LONG,
        short_bands: &SWB_OFFSET_16K_SHORT,
    }, //16K
    GASubbandInfo {
        min_srate: 11502,
        long_bands: &SWB_OFFSET_16K_LONG,
        short_bands: &SWB_OFFSET_16K_SHORT,
    }, //12K
    GASubbandInfo {
        min_srate: 9391,
        long_bands: &SWB_OFFSET_16K_LONG,
        short_bands: &SWB_OFFSET_16K_SHORT,
    }, //11.025K
    GASubbandInfo {
        min_srate: 0,
        long_bands: &SWB_OFFSET_8K_LONG,
        short_bands: &SWB_OFFSET_8K_SHORT,
    }, //8K
];
