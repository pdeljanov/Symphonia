// src/silk/decoder.rs

use std::convert::TryFrom;
use std::num::NonZeroU32;
use crate::packet::FramePacket;
use crate::silk::error::Error;
use crate::silk::frame::{Frame, FrameType, QuantizationOffsetType};
use crate::toc::{FrameSize, Parameters};
use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{Error as SymphoniaError, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::BitReaderLtr;
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

        let state = State::try_new(sample_rate, channels.count(), frame_size)?;

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

        for frame_data in frame_packet.frames() {
            self.decode_frame(frame_data)?;
        }

        return Ok(self.buffer.as_audio_buffer_ref());
    }

    fn decode_frame(&mut self, frame_data: &[u8]) -> Result<()> {
        let reader = BitReaderLtr::new(frame_data);
        let mut range_decoder = entropy::Decoder::new(reader)?;

        let frame_type = self.decode_frame_type(&mut range_decoder)?;
        let quantization_offset_type = self.decode_quantization_offset_type(&mut range_decoder)?;

        let frame_length = State::calculate_frame_length(self.state.sample_rate, self.state.frame_size)?;
        let mut frame = Frame::new(frame_type, quantization_offset_type, frame_length);


        self.decode_lsf(&mut range_decoder, &mut frame)?;
        self.decode_ltp(&mut range_decoder, &mut frame)?;
        self.decode_gains(&mut range_decoder, &mut frame)?;
        self.decode_excitation(&mut range_decoder, &mut frame)?;

        self.synthesize(&frame)?;

        Ok(())
    }

    fn decode_frame_type<R: RangeDecoder>(&self, decoder: &mut R) -> Result<FrameType> {
        let frame_type = decoder.decode_symbol_logp(1)?;
        return FrameType::try_from(frame_type as u8).map_err(SymphoniaError::from);
    }

    fn decode_quantization_offset_type<R: RangeDecoder>(&self, decoder: &mut R) -> Result<QuantizationOffsetType> {
        let offset_type = decoder.decode_symbol_logp(1)?;
        return QuantizationOffsetType::try_from(offset_type as u8).map_err(SymphoniaError::from);
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

    fn synthesize(&mut self, frame: &Frame) -> Result<()> {
        unimplemented!()
    }
}

use std::time::Duration;

pub struct State {
    sample_rate: NonZeroU32,
    channels: usize,
    frame_size: FrameSize,
    prev_samples: Vec<f32>,
}

impl State {
    pub fn try_new(sample_rate: u32, channels: usize, frame_size: FrameSize) -> Result<Self> {
        let sample_rate = NonZeroU32::new(sample_rate).ok_or(Error::UnsupportedConfig)?;
        let frame_length = Self::calculate_frame_length(sample_rate, frame_size)?;
        let prev_samples = vec![0.0; frame_length * channels];

        return Ok(Self {
            sample_rate,
            channels,
            frame_size,
            prev_samples,
        });
    }

    fn calculate_frame_length(sample_rate: NonZeroU32, frame_size: FrameSize) -> Result<usize> {
        let duration: Duration = frame_size.into();

        let samples = (sample_rate.get() as u128)
            .checked_mul(duration.as_nanos())
            .ok_or(Error::CalculationOverflow)?;

        let samples = samples
            .checked_div(1_000_000_000)
            .ok_or(Error::CalculationOverflow)?;

        return usize::try_from(samples).map_err(|_| Error::CalculationOverflow.into());
    }

    pub fn reset(&mut self) {
        self.prev_samples.fill(0.0);
    }

    pub fn update_frame_size(&mut self, new_frame_size: FrameSize) -> Result<()> {
        if self.frame_size != new_frame_size {
            self.frame_size = new_frame_size;
            let new_frame_length = Self::calculate_frame_length(self.sample_rate, new_frame_size)?;
            self.prev_samples.resize(new_frame_length * self.channels, 0.0);
        }

        return Ok(());
    }
}
