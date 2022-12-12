// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia::core::audio::{AudioBuffer, AudioBufferRef, SignalSpec, Signal};
use symphonia::core::conv::{FromSample, IntoSample};
use symphonia::core::sample::Sample;

pub struct Resampler<T> {
    resampler: rubato::FftFixedIn<f32>,
    input: AudioBuffer<f32>,
    output: Vec<Vec<f32>>,
    interleaved: Vec<T>,
}

impl<T> Resampler<T>
where
    T: Sample + FromSample<f32>,
{
    pub fn new(spec: SignalSpec, to_sample_rate: usize, duration: u64) -> Self {
        let num_channels = spec.channels.count();

        let resampler = rubato::FftFixedIn::<f32>::new(
            spec.rate as usize,
            to_sample_rate,
            duration as usize,
            2,
            num_channels,
        )
        .unwrap();

        let output = rubato::Resampler::output_buffer_allocate(&resampler);

        let input = AudioBuffer::new(duration, spec);

        Self { resampler, input, output, interleaved: Default::default() }
    }

    /// Resamples a planar/non-interleaved input.
    ///
    /// Returns the resampled samples in an interleaved format.
    pub fn resample(&mut self, input: &AudioBufferRef<'_>) -> &[T] {
        // Rubato only supports floating point samples, so convert the input buffer.
        input.convert(&mut self.input);

        // Fill the rest of the input.
        if input.frames() != self.input.capacity() {
            self.input.fill(|planes, _| {
                for plane in planes.planes().iter_mut() {
                    plane[input.frames()..].fill(0.0);
                }
                Ok(())
            }).unwrap();
        }

        // Get audio planes.
        let planes = self.input.planes();

        // Resample.
        rubato::Resampler::process_into_buffer(
            &mut self.resampler,
            planes.planes(),
            &mut self.output,
            None,
        )
        .unwrap();

        // Interleave planar samples from Rubato.
        let num_channels = self.output.len();

        self.interleaved.resize(num_channels * self.output[0].len(), T::MID);

        for (i, frame) in self.interleaved.chunks_exact_mut(num_channels).enumerate() {
            for (ch, s) in frame.iter_mut().enumerate() {
                *s = self.output[ch][i].into_sample();
            }
        }

        &self.interleaved
    }
}