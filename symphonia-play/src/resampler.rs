// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::marker::PhantomData;

use rubato::VecResampler;
use symphonia::core::audio::{Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef};
use symphonia::core::conv::{FromSample, IntoSample};
use symphonia::core::sample::Sample;

pub struct Resampler<T> {
    resampler: rubato::FftFixedIn<f32>,
    buf_in: AudioBuffer<f32>,
    buf_out: AudioBuffer<f32>,
    chunk_size: usize,
    // May take your heart.
    phantom: PhantomData<T>,
}

impl<T> Resampler<T>
where
    T: Sample + FromSample<f32> + IntoSample<f32>,
{
    fn resample_inner<'a>(&mut self, dst: &'a mut Vec<T>) -> &'a [T] {
        // Clear the output buffer.
        self.buf_out.clear();

        // Keep resampling chunks until there are not enough input frames left.
        while self.chunk_size <= self.buf_in.frames() {
            // The resampler will produce this many frames next.
            let len = self.resampler.output_frames_next();

            // If required, grow the output buffer to accomodate the output.
            let begin = self.buf_out.frames();
            self.buf_out.grow_capacity(begin + len);

            // Reserve frames for the resampler output.
            self.buf_out.render_uninit(Some(len));

            // Get slices to the required regions of the input and output buffers.
            let (read, _) = {
                let mut slices_in: smallvec::SmallVec<[&[f32]; 8]> = Default::default();
                let mut slices_out: smallvec::SmallVec<[&mut [f32]; 8]> = Default::default();

                for plane in self.buf_in.iter_planes() {
                    slices_in.push(&plane[..self.chunk_size]);
                }

                for plane in self.buf_out.iter_planes_mut() {
                    slices_out.push(&mut plane[begin..]);
                }

                // Resample a chunk.
                rubato::Resampler::process_into_buffer(
                    &mut self.resampler,
                    &slices_in,
                    &mut slices_out,
                    None,
                )
                .unwrap()
            };

            // Remove consumed samples from the input buffer.
            self.buf_in.shift(read);
        }

        // Return interleaved samples.
        self.buf_out.copy_to_vec_interleaved(dst);

        dst
    }
}

impl<T> Resampler<T>
where
    T: Sample + FromSample<f32> + IntoSample<f32>,
{
    pub fn new(spec_in: &AudioSpec, out_sample_rate: u32, chunk_size: usize) -> Self {
        let resampler = rubato::FftFixedIn::<f32>::new(
            spec_in.rate() as usize,
            out_sample_rate as usize,
            chunk_size,
            2,
            spec_in.channels().count(),
        )
        .unwrap();

        let spec_out = AudioSpec::new(out_sample_rate, spec_in.channels().clone());

        let buf_in = AudioBuffer::new(spec_in.clone(), chunk_size);
        let buf_out = AudioBuffer::new(spec_out, resampler.output_frames_max());

        Self { resampler, buf_in, buf_out, chunk_size, phantom: Default::default() }
    }

    /// Resamples a planar/non-interleaved input.
    ///
    /// Returns the resampled samples in an interleaved format.
    pub fn resample<'a>(&mut self, src: GenericAudioBufferRef<'_>, dst: &'a mut Vec<T>) -> &'a [T] {
        // Calculate the space required in the resampler input buffer.
        let begin = self.buf_in.frames();
        let num_frames = src.frames();

        // If required, grow the resampler input buffer capacity.
        self.buf_in.grow_capacity(begin + num_frames);

        // Reserve space in the resampler input buffer to accomodate the new frames.
        self.buf_in.render_uninit(Some(num_frames));

        // Copy and convert the source buffer to resampler input buffer.
        src.copy_to(&mut self.buf_in.slice_mut(begin..begin + num_frames));

        // Resample.
        self.resample_inner(dst)
    }

    /// Resample any remaining samples in the resample buffer.
    pub fn flush<'a>(&mut self, dst: &'a mut Vec<T>) -> &'a [T] {
        let partial_len = self.buf_in.frames() % self.chunk_size;

        if partial_len != 0 {
            // Pad the input buffer with silence such that the length of the input is a multiple of
            // the chunk size.
            self.buf_in.render_silence(Some(self.chunk_size - partial_len));
        }

        // Resample.
        self.resample_inner(dst)
    }
}
