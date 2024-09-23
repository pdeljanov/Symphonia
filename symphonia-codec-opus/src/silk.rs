use crate::packet::FramePacket;
use crate::toc::{AudioMode, Parameters};
use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{Error as SymphoniaError, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::BitReaderLtr;
use thiserror::Error;
use crate::range::{self, RangeDecoder};

#[derive(Error, Debug)]
pub enum Error {
    #[error("Unsupported SILK configuration")]
    UnsupportedConfig,

    #[error("Invalid VAD flags")]
    InvalidVADFlags,

    #[error("Invalid LSF coefficients")]
    InvalidLSFCoefficients,

    #[error("Decoding error: {0}")]
    DecodingError(#[from] SymphoniaError),
    
    #[error("Invalid synthesized samples")]
    InvalidSynthesizedSamples,
    
    #[error("Buffer too small")]
    BufferTooSmall,
}

impl From<Error> for SymphoniaError {
    fn from(err: Error) -> Self {
        SymphoniaError::DecodeError(Box::leak(Box::new(err.to_string())))
    }
}

pub struct Decoder {
    params: CodecParameters,
    buffer: AudioBuffer<f32>,
    state: State,
}

impl Decoder {
    pub fn new(params: &CodecParameters) -> Result<Self> {
        let sample_rate = params.sample_rate.ok_or(Error::UnsupportedConfig)?;
        let channels = params.channels.ok_or(Error::UnsupportedConfig)?;
        let max_frame_size = params.max_frames_per_packet.unwrap_or(480);

        let signal_spec = SignalSpec::new(sample_rate, channels);
        let buffer = AudioBuffer::new(max_frame_size, signal_spec);

        let state = State::new(sample_rate, channels.count());

        return Ok(Decoder {
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

        match params.audio_mode { // TODO: check should be done on higher level.
            AudioMode::Silk | AudioMode::Hybrid => (),
            AudioMode::Celt => return Err(Error::UnsupportedConfig.into()),
        }

        for frame in frame_packet.frames() {
            self.decode_frame(frame)?;
        }

        return Ok(self.buffer.as_audio_buffer_ref());
    }

    fn decode_frame(&mut self, frame_data: &[u8]) -> Result<()> {
        let reader = BitReaderLtr::new(frame_data);
        let mut range_decoder = range::Decoder::new(reader)?;

        let vad_flag = self.decode_vad_flag(&mut range_decoder)?; // FIXME: why this is unused??
        let lsf_coeffs = self.decode_lsf(&mut range_decoder)?;
        let ltp_params = self.decode_ltp(&mut range_decoder)?;
        let gains = self.decode_gains(&mut range_decoder)?;
        let excitation = self.decode_excitation(&mut range_decoder)?;

        let synthesized_samples = self.synthesize(&lsf_coeffs, &ltp_params, &gains, &excitation)?;

        self.state.save_synthesized_samples(&synthesized_samples, &mut self.buffer)?;

        return Ok(());
    }

    fn decode_vad_flag<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<bool> {
        let vad_flag = decoder.decode_symbol_logp(1)? == 1;

        return Ok(vad_flag);
    }

    fn decode_lsf<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<Vec<f32>> {
        // For simplicity, let's assume a fixed codebook and process.
        // TODO: decode LSF coefficients, this would involve VQ decoding.
        let num_coeffs = 16; // Example number of LSF coefficients.
        let mut lsf_coeffs = Vec::with_capacity(num_coeffs);

        for _ in 0..num_coeffs {
            let coeff = decoder.decode_symbol_logp(5)? as f32;
            lsf_coeffs.push(coeff);
        }

        return Ok(lsf_coeffs);
    }

    fn decode_ltp<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<LtpParameters> {
        // TODO: implement code to decode LTP parameters.
        let pitch_lag = decoder.decode_symbol_logp(7)? as usize;
        let pitch_gain = decoder.decode_symbol_logp(3)? as f32;

        return Ok(LtpParameters {
            pitch_lag,
            pitch_gain,
        });
    }

    fn decode_gains<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<Vec<f32>> {
        let num_subframes = 4; // TODO: process subframes 
        let mut gains = Vec::with_capacity(num_subframes);

        for _ in 0..num_subframes {
            let gain = decoder.decode_symbol_logp(6)? as f32;
            gains.push(gain);
        }

        Ok(gains)
    }

    fn decode_excitation<R: RangeDecoder>(&mut self, decoder: &mut R) -> Result<Vec<f32>> {
        let excitation_length = self.state.frame_length;
        let mut excitation = Vec::with_capacity(excitation_length);

        for _ in 0..excitation_length {
            let sample = decoder.decode_symbol_logp(8)? as f32;
            excitation.push(sample);
        }

        return Ok(excitation);
    }

    fn synthesize(
        &mut self,
        lsf_coeffs: &[f32],
        ltp_params: &LtpParameters,
        gains: &[f32],
        excitation: &[f32],
    ) -> Result<Vec<f32>> {
        let lpc_coeffs = self.lsf_to_lpc(lsf_coeffs)?;
        let mut synthesized = excitation.to_vec();

        for (i, sample) in synthesized.iter_mut().enumerate() {
            let subframe = i / (self.state.frame_length / gains.len());
            *sample *= gains[subframe];
        }

        self.apply_ltp(&mut synthesized, ltp_params);

        self.apply_lpc(&mut synthesized, &lpc_coeffs);

        return Ok(synthesized);
    }

    fn lsf_to_lpc(&self, lsf_coeffs: &[f32]) -> Result<Vec<f32>> {
        unimplemented!()
    }

    fn apply_ltp(&self, samples: &mut [f32], ltp_params: &LtpParameters) {
        unimplemented!()
    }

    fn apply_lpc(&self, samples: &mut [f32], lpc_coeffs: &[f32]) {
        unimplemented!()
    }
}

struct State {
    sample_rate: u32,
    channels: usize,
    frame_length: usize,
    prev_samples: Vec<f32>,
}

impl State {
    fn new(sample_rate: u32, channels: usize) -> Self {
        let frame_length = (sample_rate / 50) as usize; // Assuming 20 ms frames.
        Self {
            sample_rate,
            channels,
            frame_length,
            prev_samples: vec![0.0; frame_length * channels],
        }
    }

    fn reset(&mut self) {
        for sample in &mut self.prev_samples {
            *sample = 0.0;
        }
    }
}


impl State {
    fn save_synthesized_samples(&mut self, synthesized_samples: &[f32], buffer: &mut AudioBuffer<f32>) -> Result<()> {
        let channels = self.channels;

        if synthesized_samples.len() % channels != 0 {
            return Err(Error::InvalidSynthesizedSamples.into());
        }

        let samples_per_channel = synthesized_samples.len() / channels;

        if samples_per_channel > buffer.capacity() {
            return Err(Error::BufferTooSmall.into());
        }

        for ch in 0..channels {
            let channel = buffer.chan_mut(ch);

            if channel.len() < samples_per_channel {
                return Err(Error::BufferTooSmall.into());
            }

            synthesized_samples
                .iter()
                .skip(ch)
                .step_by(channels)
                .enumerate()
                .for_each(|(i, sample)| channel[i] = *sample);
        }

        return Ok(());
    }
}


struct LtpParameters {
    pitch_lag: usize,
    pitch_gain: f32,
}
