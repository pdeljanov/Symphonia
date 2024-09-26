use std::convert::TryFrom;
use std::num::NonZeroU32;
use crate::packet::FramePacket;
use crate::silk::error::Error;
use crate::toc::{FrameSize, Parameters};
use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{Error as SymphoniaError, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};
use crate::entropy::{self, RangeDecoder};

pub struct Decoder {
    params: CodecParameters,
    buffer: AudioBuffer<f32>,
    state: State,
}

impl Decoder {
    pub fn try_new(params: &CodecParameters) -> Result<Self> {
        let sample_rate = params.sample_rate.ok_or(Error::UnsupportedConfig)?;

        let channels = params.channels.ok_or(Error::UnsupportedConfig)?;
        let frame_size = FrameSize::default();

        let signal_spec = SignalSpec::new(sample_rate, channels);
        let buffer = AudioBuffer::new(sample_rate as u64, signal_spec);

        let state = State::try_new(sample_rate, channels, frame_size)?;

        return Ok(Self {
            params: params.clone(),
            buffer,
            state,
        });
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.buffer.clear();
    }

    pub fn decode(&mut self, packet: &Packet, params: &Parameters) -> Result<AudioBufferRef<'_>> {
        let frame_packet = FramePacket::new(&packet.data)?;

        self.state.update_frame_size(params.frame_size)?;

        let mut decoded_frames = Vec::with_capacity(frame_packet.frames.len());

        for frame_data in frame_packet.frames.iter() {
            let frame = self.decode_frame(frame_data)?;
            decoded_frames.push(frame);
        }

        self.synthesize(&decoded_frames)?;

        return Ok(self.buffer.as_audio_buffer_ref());
    }
    fn decode_frame(&mut self, data: &[u8]) -> Result<Frame> {
        let sample_count = State::calculate_frame_length(self.state.sample_rate, self.state.frame_size)?;
        let mut frame = Frame::new(FrameType::default(), false, false, sample_count);

        let reader = BitReaderLtr::new(data);
        let mut range_decoder = entropy::Decoder::new(reader)?;

        frame.frame_type = self.decode_frame_type(&mut range_decoder)?;

        frame.vad_flag = range_decoder.decode_symbol_logp(1)? != 0;

        frame.lbrr_flag = range_decoder.decode_symbol_logp(1)? != 0;

        self.decode_lsf(&mut range_decoder, &mut frame)?;
        self.decode_ltp(&mut range_decoder, &mut frame)?;
        self.decode_gains(&mut range_decoder, &mut frame)?;
        self.decode_excitation(&mut range_decoder, &mut frame)?;


        if frame.lbrr_flag {
            let num_lbrr_flags = match self.state.frame_size {
                FrameSize::Ms40 => 2,
                FrameSize::Ms60 => 3,
                _ => 0, // For 20ms or unsupported sizes, no LBRR flags
            };

            if num_lbrr_flags == 0 {
                return Err(Error::InvalidFrameSize.into());
            }

            for _ in 0..self.state.channels.count() {
                let lbrr_flags = (0..num_lbrr_flags)
                    .map(|_| range_decoder.decode_symbol_logp(1).map(|v| v != 0))
                    .collect::<Result<Vec<bool>>>()?;

                for flag in lbrr_flags {
                    if flag {
                        let lbrr_frame_data = Self::extract_lbrr_data(
                            self.state.sample_rate.get(),
                            self.state.frame_size,
                            self.state.channels,
                            data,
                        )?;

                        let lbrr_frame = self.decode_lbrr_frame(lbrr_frame_data)?;
                        frame.add_lbrr_frame(lbrr_frame);
                    }
                }
            }
        }

        return Ok(frame);
    }

    fn decode_lbrr_frame(&mut self, data: &[u8]) -> Result<Frame> {
        let sample_count = State::calculate_frame_length(self.state.sample_rate, self.state.frame_size)?;
        let mut lbrr_frame = Frame::new(FrameType::default(), false, true, sample_count);

        let reader = BitReaderLtr::new(data);
        let mut range_decoder = entropy::Decoder::new(reader)?;

        lbrr_frame.frame_type = self.decode_frame_type(&mut range_decoder)?;

        lbrr_frame.vad_flag = range_decoder.decode_symbol_logp(1)? != 0;

        lbrr_frame.lbrr_flag = range_decoder.decode_symbol_logp(1)? != 0;

        self.decode_lsf(&mut range_decoder, &mut lbrr_frame)?;
        self.decode_ltp(&mut range_decoder, &mut lbrr_frame)?;
        self.decode_gains(&mut range_decoder, &mut lbrr_frame)?;
        self.decode_excitation(&mut range_decoder, &mut lbrr_frame)?;

        return Ok(lbrr_frame);
    }

    fn decode_frame_type<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<FrameType> {
        let frame_type_code = decoder.decode_symbol_logp(1)?;
        let signal_type = SignalType::try_from(frame_type_code as u8).map_err(SymphoniaError::from)?;

        let offset_type_code = decoder.decode_symbol_logp(1)?;
        let quantization_offset_type = QuantizationOffsetType::try_from(offset_type_code as u8).map_err(SymphoniaError::from)?;

        return Ok(FrameType::new(signal_type, quantization_offset_type));
    }


    fn decode_lsf<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        unimplemented!()
    }

    fn decode_ltp<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        unimplemented!()
    }

    fn decode_gains<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        unimplemented!()
    }

    fn decode_excitation<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        unimplemented!()
    }

    fn extract_lbrr_data(sample_rate: u32, frame_size: FrameSize, channels: Channels, data: &[u8]) -> Result<&[u8]> {
        let sample_rate_nonzero = NonZeroU32::new(sample_rate).ok_or(Error::UnsupportedConfig)?;
        let sample_count = State::calculate_frame_length(sample_rate_nonzero, frame_size)?;
        let channel_count = channels.count();
        let main_frame_size = sample_count * channel_count;

        if data.len() <= main_frame_size {
            return Err(SymphoniaError::from(Error::InvalidData));
        }

        return Ok(&data[main_frame_size..]);
    }

    fn synthesize(&mut self, frames: &[Frame]) -> Result<()> {
        todo!()
    }
}

use std::time::Duration;

pub struct State {
    sample_rate: NonZeroU32,
    channels: Channels,
    frame_size: FrameSize,
    prev_samples: Vec<f32>,
}

impl State {
    pub fn try_new(sample_rate: u32, channels: Channels, frame_size: FrameSize) -> Result<Self> {
        let sample_rate = NonZeroU32::new(sample_rate).ok_or(Error::UnsupportedConfig)?;
        let frame_length = Self::calculate_frame_length(sample_rate, frame_size)?;
        let channel_count = channels.count();
        let prev_samples = vec![0.0; frame_length * channel_count];

        return Ok(Self {
            sample_rate,
            channels,
            frame_size,
            prev_samples,
        });
    }

    pub fn reset(&mut self) {
        self.prev_samples.fill(0.0);
    }

    fn calculate_frame_length(sample_rate: NonZeroU32, frame_size: FrameSize) -> Result<usize> {
        let duration = Duration::from(frame_size);

        let samples = (sample_rate.get() as u128)
            .checked_mul(duration.as_nanos())
            .ok_or(Error::CalculationOverflow)?;

        let samples = samples
            .checked_div(1_000_000_000)
            .ok_or(Error::CalculationOverflow)?;

        return usize::try_from(samples).map_err(|_| Error::CalculationOverflow.into());
    }

    fn update_frame_size(&mut self, new_frame_size: FrameSize) -> Result<()> {
        if self.frame_size != new_frame_size {
            self.frame_size = new_frame_size;
            let new_frame_length = Self::calculate_frame_length(self.sample_rate, new_frame_size)?;
            let channel_count = self.channels.count();
            self.prev_samples.resize(new_frame_length * channel_count, 0.0);
        }

        return Ok(());
    }
}


#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignalType {
    #[default]
    Inactive,
    Voiced,
    Unvoiced,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationOffsetType {
    #[default]
    High,
    Low,
}

#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct FrameType {
    signal_type: SignalType,
    quantization_offset_type: QuantizationOffsetType,
}
impl FrameType {
    pub fn new(signal_type: SignalType, quantization_offset_type: QuantizationOffsetType) -> Self {
        return Self {
            signal_type,
            quantization_offset_type,
        };
    }
}
#[derive(Debug, Default)]
pub struct Frame {
    pub frame_type: FrameType,
    pub vad_flag: bool,
    pub lbrr_flag: bool,
    pub gains: [f32; 2],
    pub nlsf: [f32; 16],
    pub pitch_lags: [u16; 2],
    pub excitation: [f32; 16],
    pub sample_count: usize,
    pub lbrr_frames: Vec<Frame>,
}

impl Frame {
    pub fn new(
        frame_type: FrameType,
        vad_flag: bool,
        lbrr_flag: bool,
        sample_count: usize,
    ) -> Self {
        return Frame {
            frame_type,
            vad_flag,
            lbrr_flag,
            gains: [0.0; 2],
            nlsf: [0.0; 16],
            pitch_lags: [0; 2],
            excitation: [0.0; 16],
            sample_count,
            lbrr_frames: Vec::new(),
        };
    }

    pub fn set_gains(&mut self, gains: &[f32; 2]) {
        self.gains = *gains;
    }

    pub fn set_nlsf(&mut self, nlsf: &[f32; 16]) {
        self.nlsf = *nlsf;
    }

    pub fn set_pitch_lags(&mut self, pitch_lags: &[u16; 2]) {
        self.pitch_lags = *pitch_lags;
    }

    pub fn set_excitation(&mut self, excitation: [f32; 16]) {
        self.excitation = excitation;
    }

    pub fn add_lbrr_frame(&mut self, lbrr_frame: Frame) {
        self.lbrr_frames.push(lbrr_frame);
    }
}

impl TryFrom<u8> for SignalType {
    type Error = Error;

    fn try_from(frame_type: u8) -> core::result::Result<Self, Self::Error> {
        return match frame_type {
            0 | 1 => Ok(SignalType::Inactive),
            2 | 3 => Ok(SignalType::Unvoiced),
            4 | 5 => Ok(SignalType::Voiced),
            _ => Err(Error::InvalidFrameType),
        };
    }
}

impl TryFrom<u8> for QuantizationOffsetType {
    type Error = Error;

    fn try_from(value: u8) -> core::result::Result<Self, Self::Error> {
        return match value {
            0 | 2 | 4 => Ok(QuantizationOffsetType::Low),
            1 | 3 | 5 => Ok(QuantizationOffsetType::High),
            _ => Err(Error::InvalidQuantizationOffsetType),
        };
    }
}

#[derive(Debug)]
pub enum StreamType {
    Coupled,
    Independent,
}

pub struct Layer {
    pub stream_type: StreamType,
    pub stream_index: usize,
    pub regular_frames: Vec<Frame>,
    pub lbrr_flag: bool,
    pub per_frame_lbrr_flags: Vec<bool>,
    pub lbrr_frames: Vec<Option<Frame>>,
}

impl Layer {
    pub fn new(/*TODO: what arguments do I need here?*/) -> Self {
        unimplemented!()
    }

    pub fn decode<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<()> {
        todo!()
    }

    pub fn reset(&mut self) {
        todo!()
    }
}
