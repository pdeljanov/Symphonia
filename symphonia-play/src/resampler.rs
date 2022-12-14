// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia::core::audio::SignalSpec;
use symphonia::core::conv::{FromSample, IntoSample};
use symphonia::core::sample::Sample;

pub struct Resampler<T> {
    resampler: rubato::FftFixedIn<f32>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    interleaved: Vec<T>,
    duration: usize,
}

impl<T> Resampler<T>
where
    T: Sample + FromSample<f32> + IntoSample<f32>,
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

        let input = vec![Vec::new(); num_channels];

        Self {
            resampler,
            input,
            output,
            duration: duration as usize,
            interleaved: Default::default(),
        }
    }

    /// Resamples a planar/non-interleaved input.
    ///
    /// Returns the resampled samples in an interleaved format.
    pub fn resample(&mut self, input: &[T]) -> Option<&[T]> {
        // Convert the input to f32 and separate into channels.
        let num_samples = input.len() / self.input.len();

        for (ch, frame) in input.chunks_exact(num_samples).enumerate() {
            self.input[ch]
                .extend::<Vec<f32>>(frame.iter().map(|sample| (*sample).into_sample()).collect());
        }

        // If the given input does not have enough samples,
        // return nothing until more samples come in.
        if self.input[0].len() < self.duration {
            return None;
        }

        // The input may have more samples than the resampler
        // was allocated for, so take only what is needed.
        let duration = self.duration;
        let input_to_resample: Vec<Vec<f32>> = self.input
            .iter_mut()
            .map(|ch| {
                let samples = ch.iter().take(duration).copied().collect();
                ch.drain(0..duration);
                samples
            })
            .collect();

        // Resample.
        rubato::Resampler::process_into_buffer(
            &mut self.resampler,
            &input_to_resample,
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

        Some(&self.interleaved)
    }

    /// Pads the remaining samples so that resampling
    /// can take place.
    pub fn flush(&mut self) -> Option<&[T]> {
        let missing = self.duration - self.input[0].len();

        // No empty samples have to be added.
        if missing <= 0 {
            return None;
        }

        for ch in 0..self.input.len() {
            self.input[ch].extend(vec![0.0; missing])
        }

        self.resample(&vec![T::MID; self.input.len()])
    }
}