// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Platform-dependant Audio Outputs

use std::result;

use symphonia::core::audio::{AudioSpec, GenericAudioBufferRef};
use symphonia::core::units::Duration;

pub trait AudioOutput {
    fn write(&mut self, decoded: GenericAudioBufferRef<'_>) -> Result<()>;
    fn flush(&mut self);
}

#[allow(dead_code)]
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum AudioOutputError {
    OpenStreamError,
    PlayStreamError,
    StreamClosedError,
}

pub type Result<T> = result::Result<T, AudioOutputError>;

#[cfg(target_os = "linux")]
mod pulseaudio {
    use super::{AudioOutput, AudioOutputError, Result};

    use symphonia::core::audio::*;
    use symphonia::core::units::Duration;

    use libpulse_binding as pulse;
    use libpulse_simple_binding as psimple;

    use log::{error, warn};

    pub struct PulseAudioOutput {
        pa: psimple::Simple,
        buf: Vec<u8>,
    }

    impl PulseAudioOutput {
        pub fn try_open(spec: &AudioSpec, _: Duration) -> Result<Box<dyn AudioOutput>> {
            let num_channels = spec.channels().count();

            assert!(num_channels < 256);

            // Create a PulseAudio stream specification.
            let pa_spec = pulse::sample::Spec {
                format: pulse::sample::Format::FLOAT32NE,
                channels: num_channels as u8,
                rate: spec.rate(),
            };

            assert!(pa_spec.is_valid());

            let pa_ch_map = map_channels_to_pa_channelmap(spec.channels());

            // PulseAudio seems to not play very short audio buffers, use these custom buffer
            // attributes for very short audio streams.
            //
            // let pa_buf_attr = pulse::def::BufferAttr {
            //     maxlength: u32::MAX,
            //     tlength: 1024,
            //     prebuf: u32::MAX,
            //     minreq: u32::MAX,
            //     fragsize: u32::MAX,
            // };

            // Create a PulseAudio connection.
            let pa_result = psimple::Simple::new(
                None,                               // Use default server
                "Symphonia Player",                 // Application name
                pulse::stream::Direction::Playback, // Playback stream
                None,                               // Default playback device
                "Music",                            // Description of the stream
                &pa_spec,                           // Signal specification
                pa_ch_map.as_ref(),                 // Channel map
                None,                               // Custom buffering attributes
            );

            match pa_result {
                Ok(pa) => Ok(Box::new(PulseAudioOutput { pa, buf: Default::default() })),
                Err(err) => {
                    error!("audio output stream open error: {err}");

                    Err(AudioOutputError::OpenStreamError)
                }
            }
        }
    }

    impl AudioOutput for PulseAudioOutput {
        fn write(&mut self, decoded: GenericAudioBufferRef<'_>) -> Result<()> {
            // Do nothing if there are no audio frames.
            if decoded.frames() == 0 {
                return Ok(());
            }

            // Interleave samples as f32 from the audio buffer into a byte buffer.
            decoded.copy_bytes_to_vec_interleaved_as::<f32>(&mut self.buf);

            // Write interleaved samples to PulseAudio.
            match self.pa.write(&self.buf) {
                Err(err) => {
                    error!("audio output stream write error: {err}");

                    Err(AudioOutputError::StreamClosedError)
                }
                _ => Ok(()),
            }
        }

        fn flush(&mut self) {
            // Flush is best-effort, ignore the returned result.
            let _ = self.pa.drain();
        }
    }

    /// Maps a set of Symphonia `Channels` to a PulseAudio channel map.
    fn map_channels_to_pa_channelmap(channels: &Channels) -> Option<pulse::channelmap::Map> {
        let mut map: pulse::channelmap::Map = Default::default();
        map.init();
        map.set_len(channels.count() as u8);

        let is_mono = channels.count() == 1;

        match channels {
            Channels::Positioned(positions) => {
                for (position, mapped) in positions.iter().zip(map.get_mut()) {
                    *mapped = map_positioned(position, is_mono)?;
                }
            }
            Channels::Discrete(count) => {
                for (index, mapped) in (0..*count).zip(map.get_mut()) {
                    *mapped = map_discrete(index)?;
                }
            }
            Channels::Custom(labels) => {
                for (label, mapped) in labels.iter().zip(map.get_mut()) {
                    *mapped = match label {
                        ChannelLabel::Positioned(position) => map_positioned(*position, is_mono)?,
                        ChannelLabel::Discrete(index) => map_discrete(*index)?,
                        ChannelLabel::Ambisonic(_) | ChannelLabel::AmbisonicBFormat(_) => {
                            // Ambisonics channels are not supported.
                            warn!("ambisonic channels not supported");
                            return None;
                        }
                        _ => {
                            // Unknown channel label type.
                            warn!("unknown channel label");
                            return None;
                        }
                    };
                }
            }
            _ => (),
        }

        Some(map)
    }

    fn map_positioned(position: Position, is_mono: bool) -> Option<pulse::channelmap::Position> {
        let pa_position = match position {
            Position::FRONT_LEFT if is_mono => pulse::channelmap::Position::Mono,
            Position::FRONT_LEFT => pulse::channelmap::Position::FrontLeft,
            Position::FRONT_RIGHT => pulse::channelmap::Position::FrontRight,
            Position::FRONT_CENTER => pulse::channelmap::Position::FrontCenter,
            Position::REAR_LEFT => pulse::channelmap::Position::RearLeft,
            Position::REAR_CENTER => pulse::channelmap::Position::RearCenter,
            Position::REAR_RIGHT => pulse::channelmap::Position::RearRight,
            Position::LFE1 => pulse::channelmap::Position::Lfe,
            Position::FRONT_LEFT_CENTER => pulse::channelmap::Position::FrontLeftOfCenter,
            Position::FRONT_RIGHT_CENTER => pulse::channelmap::Position::FrontRightOfCenter,
            Position::SIDE_LEFT => pulse::channelmap::Position::SideLeft,
            Position::SIDE_RIGHT => pulse::channelmap::Position::SideRight,
            Position::TOP_CENTER => pulse::channelmap::Position::TopCenter,
            Position::TOP_FRONT_LEFT => pulse::channelmap::Position::TopFrontLeft,
            Position::TOP_FRONT_CENTER => pulse::channelmap::Position::TopFrontCenter,
            Position::TOP_FRONT_RIGHT => pulse::channelmap::Position::TopFrontRight,
            Position::TOP_REAR_LEFT => pulse::channelmap::Position::TopRearLeft,
            Position::TOP_REAR_CENTER => pulse::channelmap::Position::TopRearCenter,
            Position::TOP_REAR_RIGHT => pulse::channelmap::Position::TopRearRight,
            _ => {
                // If a Symphonia channel cannot map to a PulseAudio position then return `None`
                // because PulseAudio will not be able to open a stream with invalid channels.
                warn!("failed to map positioned channel {position:?} to output");
                return None;
            }
        };
        Some(pa_position)
    }

    fn map_discrete(index: u16) -> Option<pulse::channelmap::Position> {
        let pa_position = match index {
            0 => pulse::channelmap::Position::Aux0,
            1 => pulse::channelmap::Position::Aux1,
            2 => pulse::channelmap::Position::Aux2,
            3 => pulse::channelmap::Position::Aux3,
            4 => pulse::channelmap::Position::Aux4,
            5 => pulse::channelmap::Position::Aux5,
            6 => pulse::channelmap::Position::Aux6,
            7 => pulse::channelmap::Position::Aux7,
            8 => pulse::channelmap::Position::Aux8,
            9 => pulse::channelmap::Position::Aux9,
            10 => pulse::channelmap::Position::Aux10,
            11 => pulse::channelmap::Position::Aux11,
            12 => pulse::channelmap::Position::Aux12,
            13 => pulse::channelmap::Position::Aux13,
            14 => pulse::channelmap::Position::Aux14,
            15 => pulse::channelmap::Position::Aux15,
            16 => pulse::channelmap::Position::Aux16,
            17 => pulse::channelmap::Position::Aux17,
            18 => pulse::channelmap::Position::Aux18,
            19 => pulse::channelmap::Position::Aux19,
            20 => pulse::channelmap::Position::Aux20,
            21 => pulse::channelmap::Position::Aux21,
            22 => pulse::channelmap::Position::Aux22,
            23 => pulse::channelmap::Position::Aux23,
            24 => pulse::channelmap::Position::Aux24,
            25 => pulse::channelmap::Position::Aux25,
            26 => pulse::channelmap::Position::Aux26,
            27 => pulse::channelmap::Position::Aux27,
            28 => pulse::channelmap::Position::Aux28,
            29 => pulse::channelmap::Position::Aux29,
            30 => pulse::channelmap::Position::Aux30,
            31 => pulse::channelmap::Position::Aux31,
            _ => {
                // If a Symphonia channel cannot map to a PulseAudio position then return `None`
                // because PulseAudio will not be able to open a stream with invalid channels.
                warn!("failed to map discrete channel {index:?} to output");
                return None;
            }
        };
        Some(pa_position)
    }
}

#[cfg(not(target_os = "linux"))]
mod cpal {
    use crate::resampler::Resampler;

    use super::{AudioOutput, AudioOutputError, Result};

    use symphonia::core::audio::conv::{ConvertibleSample, IntoSample};
    use symphonia::core::audio::{AudioSpec, GenericAudioBufferRef};
    use symphonia::core::units::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use rb::*;

    use log::{error, info};

    pub struct CpalAudioOutput;

    trait AudioOutputSample:
        cpal::Sample + ConvertibleSample + IntoSample<f32> + std::marker::Send + 'static
    {
    }

    impl AudioOutputSample for f32 {}
    impl AudioOutputSample for i16 {}
    impl AudioOutputSample for u16 {}

    impl CpalAudioOutput {
        pub fn try_open(spec: &AudioSpec, duration: Duration) -> Result<Box<dyn AudioOutput>> {
            // Get default host.
            let host = cpal::default_host();

            // Get the default audio output device.
            let device = match host.default_output_device() {
                Some(device) => device,
                _ => {
                    error!("failed to get default audio output device");
                    return Err(AudioOutputError::OpenStreamError);
                }
            };

            let config = match device.default_output_config() {
                Ok(config) => config,
                Err(err) => {
                    error!("failed to get default audio output device config: {}", err);
                    return Err(AudioOutputError::OpenStreamError);
                }
            };

            // Select proper playback routine based on sample format.
            match config.sample_format() {
                cpal::SampleFormat::F32 => {
                    CpalAudioOutputImpl::<f32>::try_open(spec, duration, &device)
                }
                cpal::SampleFormat::I16 => {
                    CpalAudioOutputImpl::<i16>::try_open(spec, duration, &device)
                }
                cpal::SampleFormat::U16 => {
                    CpalAudioOutputImpl::<u16>::try_open(spec, duration, &device)
                }
            }
        }
    }

    struct CpalAudioOutputImpl<T: AudioOutputSample>
    where
        T: AudioOutputSample,
    {
        ring_buf_producer: rb::Producer<T>,
        output: Vec<T>,
        stream: cpal::Stream,
        resampler: Option<Resampler<T>>,
    }

    impl<T: AudioOutputSample> CpalAudioOutputImpl<T> {
        pub fn try_open(
            spec: &AudioSpec,
            duration: Duration,
            device: &cpal::Device,
        ) -> Result<Box<dyn AudioOutput>> {
            let num_channels = spec.channels().count();

            // Output audio stream config.
            let config = if cfg!(not(target_os = "windows")) {
                cpal::StreamConfig {
                    channels: num_channels as cpal::ChannelCount,
                    sample_rate: cpal::SampleRate(spec.rate()),
                    buffer_size: cpal::BufferSize::Default,
                }
            }
            else {
                // Use the default config for Windows.
                device
                    .default_output_config()
                    .expect("Failed to get the default output config.")
                    .config()
            };

            // Create a ring buffer with a capacity for up-to 200ms of audio.
            let ring_len = ((200 * config.sample_rate.0 as usize) / 1000) * num_channels;

            let ring_buf = SpscRb::new(ring_len);
            let (ring_buf_producer, ring_buf_consumer) = (ring_buf.producer(), ring_buf.consumer());

            let stream_result = device.build_output_stream(
                &config,
                move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                    // Write out as many samples as possible from the ring buffer to the audio
                    // output.
                    let written = ring_buf_consumer.read(data).unwrap_or(0);

                    // Mute any remaining samples.
                    data[written..].iter_mut().for_each(|s| *s = T::MID);
                },
                move |err| error!("audio output error: {}", err),
            );

            if let Err(err) = stream_result {
                error!("audio output stream open error: {}", err);

                return Err(AudioOutputError::OpenStreamError);
            }

            let stream = stream_result.unwrap();

            // Start the output stream.
            if let Err(err) = stream.play() {
                error!("audio output stream play error: {}", err);

                return Err(AudioOutputError::PlayStreamError);
            }

            let resampler = if spec.rate() != config.sample_rate.0 {
                info!("resampling {} Hz to {} Hz", spec.rate(), config.sample_rate.0);
                Some(Resampler::new(spec, config.sample_rate.0, duration.get() as usize))
            }
            else {
                None
            };

            Ok(Box::new(CpalAudioOutputImpl {
                ring_buf_producer,
                output: Default::default(),
                stream,
                resampler,
            }))
        }
    }

    impl<T: AudioOutputSample> AudioOutput for CpalAudioOutputImpl<T> {
        fn write(&mut self, decoded: GenericAudioBufferRef<'_>) -> Result<()> {
            // Do nothing if there are no audio frames.
            if decoded.frames() == 0 {
                return Ok(());
            }

            let mut samples = if let Some(resampler) = &mut self.resampler {
                // Resampling is required. The resampler will return interleaved samples in the
                // correct sample format.
                resampler.resample(decoded, &mut self.output)
            }
            else {
                // Resampling is not required. Interleave the sample for cpal using a sample buffer.
                decoded.copy_to_vec_interleaved(&mut self.output);
                &self.output[..]
            };

            // Write all samples to the ring buffer.
            while let Some(written) = self.ring_buf_producer.write_blocking(samples) {
                samples = &samples[written..];
            }

            Ok(())
        }

        fn flush(&mut self) {
            // If there is a resampler, then it may need to be flushed
            // depending on the number of samples it has.
            if let Some(resampler) = &mut self.resampler {
                let mut remaining_samples = resampler.flush(&mut self.output);

                while let Some(written) = self.ring_buf_producer.write_blocking(remaining_samples) {
                    remaining_samples = &remaining_samples[written..];
                }
            }

            // Flush is best-effort, ignore the returned result.
            let _ = self.stream.pause();
        }
    }
}

#[cfg(target_os = "linux")]
pub fn try_open(spec: &AudioSpec, duration: Duration) -> Result<Box<dyn AudioOutput>> {
    pulseaudio::PulseAudioOutput::try_open(spec, duration)
}

#[cfg(not(target_os = "linux"))]
pub fn try_open(spec: &AudioSpec, duration: Duration) -> Result<Box<dyn AudioOutput>> {
    cpal::CpalAudioOutput::try_open(spec, duration)
}
