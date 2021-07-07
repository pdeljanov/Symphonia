// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `audio` module provides primitives for working with multi-channel audio buffers of varying
//! sample formats.

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::slice;
use std::vec::Vec;

use arrayvec::ArrayVec;
use bitflags::bitflags;

use crate::conv::{ConvertibleSample, IntoSample};
use crate::errors::Result;
use crate::sample::{Sample, i24, u24};
use crate::units::Duration;

bitflags! {
    /// Channels is a bit mask of all channels contained in a signal.
    #[derive(Default)]
    pub struct Channels: u32 {
        /// Front-left (left) or the Mono channel.
        const FRONT_LEFT         = 0x0000_0001;
        /// Front-right (right) channel.
        const FRONT_RIGHT        = 0x0000_0002;
        /// Front-centre (centre) channel.
        const FRONT_CENTRE       = 0x0000_0004;
        /// Rear-left (surround rear left) channel.
        const REAR_LEFT          = 0x0000_0008;
        /// Rear-centre (surround rear centre) channel.
        const REAR_CENTRE        = 0x0000_0010;
        /// Rear-right (surround rear right) channel.
        const REAR_RIGHT         = 0x0000_0020;
        /// Low frequency channel 1.
        const LFE1               = 0x0000_0040;
        /// Front left-of-centre (left center) channel.
        const FRONT_LEFT_CENTRE  = 0x0000_0080;
        /// Front right-of-centre (right center) channel.
        const FRONT_RIGHT_CENTRE = 0x0000_0100;
        /// Rear left-of-centre channel.
        const REAR_LEFT_CENTRE   = 0x0000_0200;
        /// Rear right-of-centre channel.
        const REAR_RIGHT_CENTRE  = 0x0000_0400;
        /// Front left-wide channel.
        const FRONT_LEFT_WIDE    = 0x0000_0800;
        /// Front right-wide channel.
        const FRONT_RIGHT_WIDE   = 0x0000_1000;
        /// Front left-high channel.
        const FRONT_LEFT_HIGH    = 0x0000_2000;
        /// Front centre-high channel.
        const FRONT_CENTRE_HIGH  = 0x0000_4000;
        /// Front right-high channel.
        const FRONT_RIGHT_HIGH   = 0x0000_8000;
        /// Low frequency channel 2.
        const LFE2               = 0x0001_0000;
        /// Side left (surround left) channel.
        const SIDE_LEFT          = 0x0002_0000;
        /// Side right (surround right) channel.
        const SIDE_RIGHT         = 0x0004_0000;
        /// Top centre channel.
        const TOP_CENTRE         = 0x0008_0000;
        /// Top front-left channel.
        const TOP_FRONT_LEFT     = 0x0010_0000;
        /// Top centre channel.
        const TOP_FRONT_CENTRE   = 0x0020_0000;
        /// Top front-right channel.
        const TOP_FRONT_RIGHT    = 0x0040_0000;
        /// Top rear-left channel.
        const TOP_REAR_LEFT      = 0x0080_0000;
        /// Top rear-centre channel.
        const TOP_REAR_CENTRE    = 0x0100_0000;
        /// Top rear-right channel.
        const TOP_REAR_RIGHT     = 0x0200_0000;
    }
}

impl Channels {
    /// Gets the number of channels.
    pub fn count(self) -> usize {
        self.bits.count_ones() as usize
    }
}

impl fmt::Display for Channels {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#032b}", self.bits)
    }
}

/// `Layout` describes common audio channel configurations.
#[derive(Copy, Clone, Debug)]
pub enum Layout {
    /// Single centre channel.
    Mono,
    /// Left and Right channels.
    Stereo,
    /// Left and Right channels with a single low-frequency channel.
    TwoPointOne,
    /// Front Left and Right, Rear Left and Right, and a single low-frequency channel.
    FivePointOne,
}

impl Layout {

    /// Converts a channel `Layout` into a `Channels` bit mask.
    fn into_channels(self) -> Channels {
        match self {
            Layout::Mono => {
                Channels::FRONT_LEFT
            },
            Layout::Stereo => {
                Channels::FRONT_LEFT | Channels::FRONT_RIGHT
            },
            Layout::TwoPointOne => {
                Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::LFE1
            },
            Layout::FivePointOne => {
                Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::FRONT_CENTRE
                    | Channels::REAR_LEFT
                    | Channels::REAR_RIGHT
                    | Channels::LFE1
            }
        }
    }

}

/// `SignalSpec` describes the characteristics of a Signal.
#[derive(Copy, Clone, PartialEq)]
pub struct SignalSpec {
    /// The signal sampling rate in hertz (Hz).
    pub rate: u32,

    /// The channel assignments of the signal. The order of the channels in the vector is the order
    /// in which each channel sample is stored in a frame.
    pub channels: Channels,
}

impl SignalSpec {
    pub fn new(rate: u32, channels: Channels) -> Self {
        SignalSpec { rate, channels }
    }

    pub fn new_with_layout(rate: u32, layout: Layout) -> Self {
        SignalSpec {
            rate,
            channels: layout.into_channels(),
        }
    }
}


/// `WriteSample` provides a typed interface for converting a sample from it's in-memory type to its
/// StreamType.
pub trait WriteSample : Sample {
    fn write(sample: Self, dest: &mut SampleWriter<Self>);
}

impl WriteSample for u8 {
    #[inline(always)]
    fn write(sample: u8, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for i8 {
    #[inline(always)]
    fn write(sample: i8, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for u16 {
    #[inline(always)]
    fn write(sample: u16, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for i16 {
    #[inline(always)]
    fn write(sample: i16, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for u24 {
    #[inline(always)]
    fn write(sample: u24, writer: &mut SampleWriter<Self>) {
        writer.write(sample.to_ne_bytes());
    }
}

impl WriteSample for i24 {
    #[inline(always)]
    fn write(sample: i24, writer: &mut SampleWriter<Self>) {
        writer.write(sample.to_ne_bytes());
    }
}

impl WriteSample for u32 {
    #[inline(always)]
    fn write(sample: u32, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for i32 {
    #[inline(always)]
    fn write(sample: i32, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for f32 {
    #[inline(always)]
    fn write(sample: f32, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for f64 {
    #[inline(always)]
    fn write(sample: f64, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

/// `AudioPlanes` provides immutable slices to each audio channel (plane) contained in a signal.
pub struct AudioPlanes<'a, S: 'a + Sample> {
    planes: ArrayVec<&'a [S], 32>,
}

impl<'a, S : Sample> AudioPlanes<'a, S> {
    fn new() -> Self {
        AudioPlanes { planes: ArrayVec::new() }
    }

    /// Gets all the audio planes.
    pub fn planes(&self) -> &[&'a [S]] {
        &self.planes
    }
}

/// `AudioPlanesMut` provides mutable slices to each audio channel (plane) contained in a signal.
pub struct AudioPlanesMut<'a, S: 'a + Sample> {
    planes: ArrayVec<&'a mut [S], 32>,
}

impl<'a, S : Sample> AudioPlanesMut<'a, S> {
    fn new() -> Self {
        AudioPlanesMut { planes: ArrayVec::new() }
    }

    /// Gets all the audio planes.
    pub fn planes(&mut self) -> &mut [&'a mut [S]] {
        &mut self.planes
    }
}

/// `AudioBuffer` is a container for multi-channel planar audio sample data. An `AudioBuffer` is
/// characterized by the duration (capacity), and audio specification (channels and sample rate).
/// The capacity of an `AudioBuffer` is the maximum number of samples the buffer may store per
/// channel. Manipulation of samples is accomplished through the Signal trait or direct buffer
/// manipulation.
#[derive(Clone)]
pub struct AudioBuffer<S : Sample> {
    buf: Vec<S>,
    spec: SignalSpec,
    n_frames: usize,
    n_capacity: usize,
}

impl<S : Sample> AudioBuffer<S> {
    /// Instantiate a new `AudioBuffer` using the specified signal specification and of the given
    /// duration.
    pub fn new(duration: Duration, spec: SignalSpec) -> Self {
        let n_sample_capacity = duration * spec.channels.count() as u64;

        // Practically speaking, it is not possible to allocate more than usize samples.
        assert!(n_sample_capacity <= usize::max_value() as u64);

        // Allocate memory for the sample data and default initialize the sample to silence.
        let buf = vec![S::default(); n_sample_capacity as usize];

        AudioBuffer {
            buf,
            spec,
            n_frames: 0,
            n_capacity: duration as usize,
        }
    }

    /// Instantiates an unused `AudioBuffer`. An unused `AudioBuffer` will not allocate any memory,
    /// has a sample rate of 0, and no audio channels.
    pub fn unused() -> Self {
        AudioBuffer {
            buf: Vec::with_capacity(0),
            spec: SignalSpec::new(0, Channels::empty()),
            n_frames: 0,
            n_capacity: 0,
        }
    }

    /// Returns `true` if the `AudioBuffer` is unused.
    pub fn is_unused(&self) -> bool {
        self.n_capacity == 0
    }

    /// Gets the signal specification for the buffer.
    pub fn spec(&self) -> &SignalSpec {
        &self.spec
    }

    /// Gets the total capacity of the buffer. The capacity is the maximum number of frames a buffer
    /// can store.
    pub fn capacity(&self) -> usize {
        self.n_capacity
    }

    /// Gets immutable references to all audio planes (channels) within the audio buffer.
    ///
    /// Note: This is not a cheap operation. It is advisable that this call is only used when
    /// operating on batches of frames. Generally speaking, it is almost always better to use
    /// `chan()` to selectively choose the plane to read.
    pub fn planes(&self) -> AudioPlanes<S> {
        // Fill the audio planes structure with references to the written portion of each audio
        // plane.
        let mut planes = AudioPlanes::new();

        for channel in self.buf.chunks_exact(self.n_capacity) {
            planes.planes.push(&channel[..self.n_frames]);
        }

        planes
    }

    /// Gets mutable references to all audio planes (channels) within the buffer.
    ///
    /// Note: This is not a cheap operation. It is advisable that this call is only used when
    /// mutating batches of frames. Generally speaking, it is almost always better to use
    /// `render()`, `fill()`, `chan_mut()`, and `chan_pair_mut()` to mutate the buffer.
    pub fn planes_mut(&mut self) -> AudioPlanesMut<S> {
        // Fill the audio planes structure with references to the written portion of each audio
        // plane.
        let mut planes = AudioPlanesMut::new();

        for channel in self.buf.chunks_exact_mut(self.n_capacity) {
            planes.planes.push(&mut channel[..self.n_frames]);
        }

        planes
    }

    /// Converts the contents of an AudioBuffer into an equivalent destination AudioBuffer of a
    /// different type. If the types are the same then this is a copy operation.
    pub fn convert<T: Sample>(&self, dest: &mut AudioBuffer<T>)
    where
        S: IntoSample<T>
    {
        assert!(dest.n_frames == self.n_frames);
        assert!(dest.n_capacity == self.n_capacity);
        assert!(dest.spec == self.spec);

        for c in 0..self.spec.channels.count() {
            let begin = c * self.n_capacity;
            let end = begin + self.n_frames;

            for (d, s) in dest.buf[begin..end].iter_mut().zip(&self.buf[begin..end]) {
                *d = (*s).into_sample();
            }
        }
    }

    /// Makes an equivalent AudioBuffer of a different type.
    pub fn make_equivalent<E: Sample>(&self) -> AudioBuffer<E> {
        AudioBuffer::<E>::new(self.n_capacity as Duration, self.spec)
    }
}

/// `AudioBufferRef` is a copy-on-write reference to an AudioBuffer of any type.
pub enum AudioBufferRef<'a> {
    F32(Cow<'a, AudioBuffer<f32>>),
    S32(Cow<'a, AudioBuffer<i32>>),
}

impl<'a> AudioBufferRef<'a> {
    pub fn spec(&self) -> &SignalSpec {
        match self {
            AudioBufferRef::F32(buf) => buf.spec(),
            AudioBufferRef::S32(buf) => buf.spec(),
        }
    }

    pub fn capacity(&self) -> usize {
        match self {
            AudioBufferRef::F32(buf) => buf.capacity(),
            AudioBufferRef::S32(buf) => buf.capacity(),
        }
    }
}

/// `AsAudioBufferRef` is a trait implemented for `AudioBuffer`s that may be referenced in an
/// `AudioBufferRef`.
pub trait AsAudioBufferRef {
    fn as_audio_buffer_ref(&self) -> AudioBufferRef;
}

impl AsAudioBufferRef for AudioBuffer<f32> {
    fn as_audio_buffer_ref(&self) -> AudioBufferRef {
        AudioBufferRef::F32(Cow::Borrowed(self))
    }
}

impl AsAudioBufferRef for AudioBuffer<i32> {
    fn as_audio_buffer_ref(&self) -> AudioBufferRef {
        AudioBufferRef::S32(Cow::Borrowed(self))
    }
}

/// The `Signal` trait provides methods for rendering and transforming contiguous buffers of audio
/// data.
pub trait Signal<S : Sample> {
    /// Gets the number of actual frames written to the buffer. Conversely, this also is the number
    /// of written samples in any one channel.
    fn frames(&self) -> usize;

    /// Clears all written frames from the buffer. This is a cheap operation and does not zero the
    /// underlying audio data.
    fn clear(&mut self);

    /// Gets an immutable reference to all the written samples in the specified channel.
    fn chan(&self, channel: usize) -> &[S];

    /// Gets a mutable reference to all the written samples in the specified channel.
    fn chan_mut(&mut self, channel: usize) -> &mut [S];

    /// Gets two mutable references to two different channels.
    fn chan_pair_mut(&mut self, first: usize, second: usize) -> (&mut [S], &mut [S]);

    /// Renders a reserved number of frames. This is a cheap operation and simply advances the frame
    /// counter. The underlying audio data is not modified and should be overwritten through other
    /// means.
    ///
    /// If `n_frames` is `None`, the remaining number of samples will be used. If `n_frames` is too
    /// large, this function will assert.
    fn render_reserved(&mut self, n_frames: Option<usize>);

    /// Renders a number of frames using the provided render function. The number of frames to
    /// render is specified by `n_frames`. If `n_frames` is `None`, the remaining number of frames
    /// in the buffer will be rendered. If the render function returns an error, the render
    /// operation is terminated prematurely.
    fn render<'a, F>(&'a mut self, n_frames: Option<usize>, render: F) -> Result<()>
    where
        F: FnMut(&mut AudioPlanesMut<'a, S>, usize) -> Result<()>;

    /// Clears, and then renders the entire buffer using the fill function. This is a convenience
    /// wrapper around `render` and exhibits the same behaviour as `render` in regards to the fill
    /// function.
    #[inline]
    fn fill<'a, F>(&'a mut self, fill: F) -> Result<()>
    where
        F: FnMut(&mut AudioPlanesMut<'a, S>, usize) -> Result<()>
    {
        self.clear();
        self.render(None, fill)
    }

    /// Transforms every written sample in the signal using the transformation function provided.
    /// This function does not guarantee an order in which the samples are transformed.
    fn transform<F>(&mut self, f: F)
    where
        F: Fn(S) -> S;
}

impl<S: Sample> Signal<S> for AudioBuffer<S> {

    fn clear(&mut self) {
        self.n_frames = 0;
    }

    fn frames(&self) -> usize {
        self.n_frames
    }

    fn chan(&self, channel: usize) -> &[S]{
        let start = channel * self.n_capacity;
        let end = start + self.n_frames;

        // Do not exceed the audio buffer.
        assert!(end <= self.buf.len());

        &self.buf[start..end]
    }

    fn chan_mut(&mut self, channel: usize) -> &mut [S] {
        let start = channel * self.n_capacity;
        let end = start + self.n_frames;

        // Do not exceed the audio buffer.
        assert!(end <= self.buf.len());

        &mut self.buf[start..end]
    }

    fn chan_pair_mut(&mut self, first: usize, second: usize) -> (&mut [S], &mut [S]) {
        // Both channels in the pair must be unique.
        assert!(first != second);

        let first_idx = self.n_capacity * first;
        let second_idx = self.n_capacity * second;

        if first_idx < second_idx {
            let (a, b) = self.buf.split_at_mut(second_idx);

            (&mut a[first_idx..first_idx + self.n_frames], &mut b[..self.n_frames])
        }
        else {
            let (a, b) = self.buf.split_at_mut(first_idx);
            
            (&mut b[..self.n_frames], &mut a[second_idx..second_idx + self.n_frames])
        }
    }

    fn render_reserved(&mut self, n_frames: Option<usize>) {
        let n_reserved_frames = n_frames.unwrap_or(self.n_capacity - self.n_frames);
        // Do not render past the end of the audio buffer.
        assert!(self.n_frames + n_reserved_frames <= self.n_capacity);
        self.n_frames += n_reserved_frames;
    }

    fn render<'a, F>(&'a mut self, n_frames: Option<usize>, mut render: F) -> Result<()>
    where
        F: FnMut(&mut AudioPlanesMut<'a, S>, usize) -> Result<()>
    {
        // The number of frames to be rendered is the amount requested, if specified, or the
        // remainder of the audio buffer.
        let n_render_frames = n_frames.unwrap_or(self.n_capacity - self.n_frames);

        // Do not render past the end of the audio buffer.
        let end = self.n_frames + n_render_frames;
        assert!(end <= self.n_capacity);

        // At this point, n_render_frames can be considered "reserved". Create an audio plane
        // structure and fill each plane entry with a reference to the "reserved" samples in each
        // channel respectively.
        let mut planes = AudioPlanesMut::new();

        for channel in self.buf.chunks_exact_mut(self.n_capacity) {
            planes.planes.push(&mut channel[self.n_frames..end]);
        }

        // Attempt to render the into the reserved frames, one-by-one, exiting only if there is an
        // error in the render function.
        while self.n_frames < end {
            render(&mut planes, self.n_frames)?;
            self.n_frames += 1;
        }

        Ok(())
    }

    fn transform<F>(&mut self, f: F)
    where
        F: Fn(S) -> S
    {
        debug_assert!(self.n_frames <= self.n_capacity);

        // Apply the transformation function over each sample in each plane.
        for plane in self.buf.chunks_mut(self.n_capacity) {
            for sample in &mut plane[0..self.n_frames] {
                *sample = f(*sample);
            }
        }
    }

}

/// A `SampleBuffer`, is a sample oriented buffer. It is agnostic to the ordering/layout of samples
/// within the buffer. `SampleBuffer` is mean't for safely importing and exporting sample data to
/// and from Symphonia using the sample's in-memory data-type.
pub struct SampleBuffer<S: Sample> {
    buf: Vec<S>,
    n_written: usize,
}

impl<S: Sample> SampleBuffer<S> {
    /// Instantiate a new `SampleBuffer` using the specified signal specification and of the given
    /// duration.
    pub fn new(duration: Duration, spec: SignalSpec) -> SampleBuffer<S> {
        let n_samples = duration * spec.channels.count() as u64;

        // Practically speaking, it is not possible to allocate more than usize samples.
        assert!(n_samples <= usize::max_value() as u64);

        SampleBuffer {
            buf: vec![S::MID; n_samples as usize],
            n_written: 0,
        }
    }

    /// Gets the number of written samples.
    pub fn len(&self) -> usize {
        self.n_written
    }

    /// Gets an immutable slice of all written samples.
    pub fn samples(&self) -> &[S] {
        &self.buf[..self.n_written]
    }

    /// Gets the maximum number of samples the `SampleBuffer` may store.
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Copies all audio data from the source `AudioBufferRef` in planar channel order into the
    /// `SampleBuffer`. The two buffers must be equivalent.
    pub fn copy_planar_ref(&mut self, src: AudioBufferRef)
    where
        S: ConvertibleSample,
    {
        match src {
            AudioBufferRef::F32(buf) => self.copy_planar_typed(&buf),
            AudioBufferRef::S32(buf) => self.copy_planar_typed(&buf),
        }
    }

    /// Copies all audio data from a source `AudioBuffer` into the `SampleBuffer` in planar
    /// channel order. The two buffers must be equivalent.
    pub fn copy_planar_typed<F>(&mut self, src: &AudioBuffer<F>)
    where
        F: Sample + IntoSample<S>,
    {
        let n_frames = src.frames();
        let n_channels = src.spec.channels.count();
        let n_samples = n_frames * n_channels;

        // Ensure that the capacity of the sample buffer is greater than or equal to the number
        // of samples that will be copied from the source buffer.
        assert!(self.capacity() >= n_samples);

        for ch in 0..n_channels {
            let ch_slice = src.chan(ch);

            for (dst, src) in self.buf[ch * n_frames..].iter_mut().zip(ch_slice) {
                *dst = (*src).into_sample();
            }
        }

        // Commit the written samples.
        self.n_written = n_samples;
    }

    /// Copies all audio data from the source `AudioBufferRef` in interleaved channel order into the
    /// `SampleBuffer`. The two buffers must be equivalent.
    pub fn copy_interleaved_ref(&mut self, src: AudioBufferRef)
    where
        S: ConvertibleSample,
    {
        match src {
            AudioBufferRef::F32(buf) => self.copy_interleaved_typed(&buf),
            AudioBufferRef::S32(buf) => self.copy_interleaved_typed(&buf),
        }
    }

    /// Copies all audio samples from a source `AudioBuffer` into the `SampleBuffer` in interleaved
    /// channel order. The two buffers must be equivalent.
    pub fn copy_interleaved_typed<F>(&mut self, src: &AudioBuffer<F>)
    where
        F: Sample + IntoSample<S>,
    {
        let n_channels = src.spec.channels.count();
        let n_samples = src.frames() * n_channels;

        // Ensure that the capacity of the sample buffer is greater than or equal to the number
        // of samples that will be copied from the source buffer.
        assert!(self.capacity() >= n_samples);

        // Interleave the source buffer channels into the sample buffer.
        for ch in 0..n_channels {
            let ch_slice = src.chan(ch);

            for (dst, src) in self.buf[ch..].iter_mut().step_by(n_channels).zip(ch_slice) {
                *dst = (*src).into_sample();
            }
        }

        // Commit the written samples.
        self.n_written = n_samples;
    }
}

/// A `RawSampleBuffer`, is a byte-oriented sample buffer. All samples copied to this buffer are
/// converted into their packed data-type and stored as a stream of bytes. `RawSampleBuffer` is
/// mean't for safely importing and exporting sample data to and from Symphonia as raw bytes.
pub struct RawSampleBuffer<S: Sample + WriteSample> {
    buf: Vec<u8>,
    n_written: usize,
    // Might take your heart.
    sample_format: PhantomData<S>,
}

impl<S: Sample + WriteSample> RawSampleBuffer<S> {
    /// Instantiate a new `RawSampleBuffer` using the specified signal specification and of the given
    /// duration.
    pub fn new(duration: Duration, spec: SignalSpec) -> RawSampleBuffer<S> {
        let n_samples = duration * spec.channels.count() as u64;

        // Practically speaking, it is not possible to allocate more than usize samples.
        assert!(n_samples <= usize::max_value() as u64);

        // Allocate enough memory for all the samples.
        let byte_length = n_samples as usize * mem::size_of::<S::StreamType>();
        let buf = vec![0u8; byte_length];

        RawSampleBuffer {
            buf,
            n_written: 0,
            sample_format: PhantomData,
        }
    }

    /// Gets the number of written samples.
    pub fn len(&self) -> usize {
        self.n_written
    }

    /// Gets the maximum number of samples the `RawSampleBuffer` may store.
    pub fn capacity(&self) -> usize {
        self.buf.len() / mem::size_of::<S>()
    }

    /// Gets an immutable slice to the bytes of the sample's written in the `RawSampleBuffer`.
    pub fn as_bytes(&self) -> &[u8] {
        let end = self.n_written * mem::size_of::<S::StreamType>();
        &self.buf[..end]
    }

    /// Copies all audio data from the source `AudioBufferRef` in planar channel order into the
    /// `RawSampleBuffer`. The two buffers must be equivalent.
    pub fn copy_planar_ref(&mut self, src: AudioBufferRef)
    where
        S: ConvertibleSample,
    {
        match src {
            AudioBufferRef::F32(buf) => self.copy_planar_typed(&buf),
            AudioBufferRef::S32(buf) => self.copy_planar_typed(&buf),
        }
    }

    /// Copies all audio data from a source `AudioBuffer` that is of a different sample format type
    /// than that of the `RawSampleBuffer` in planar channel order. The two buffers must be equivalent.
    pub fn copy_planar_typed<F>(&mut self, src: &AudioBuffer<F>)
    where
        F: Sample + IntoSample<S>,
    {
        let n_frames = src.n_frames;
        let n_channels = src.spec.channels.count();
        let n_samples = n_frames * n_channels;

        // Ensure that the capacity of the sample buffer is greater than or equal to the number
        // of samples that will be copied from the source buffer.
        assert!(self.capacity() >= n_samples);

        let mut writer = SampleWriter::from_buf(n_samples, self);

        for ch in 0..n_channels {
            let begin = ch * src.n_capacity;
            for sample in &src.buf[begin..(begin + n_frames)] {
                S::write((*sample).into_sample(), &mut writer);
            }
        }
    }

    /// Copies all audio data from the source `AudioBuffer` to the `RawSampleBuffer` in planar order.
    /// The two buffers must be equivalent.
    pub fn copy_planar(&mut self, src: &AudioBuffer<S>) {
        let n_frames = src.n_frames;
        let n_channels = src.spec.channels.count();
        let n_samples = n_frames * n_channels;

        // Ensure that the capacity of the sample buffer is greater than or equal to the number
        // of samples that will be copied from the source buffer.
        assert!(self.capacity() >= n_samples);

        let mut writer = SampleWriter::from_buf(n_samples, self);

        for ch in 0..n_channels {
            let begin = ch * src.n_capacity;
            for sample in &src.buf[begin..(begin + n_frames)] {
                S::write(*sample, &mut writer);
            }
        }
    }

    /// Copies all audio data from the source `AudioBufferRef` in interleaved channel order into the
    /// `RawSampleBuffer`. The two buffers must be equivalent.
    pub fn copy_interleaved_ref(&mut self, src: AudioBufferRef)
    where
        S: ConvertibleSample,
    {
        match src {
            AudioBufferRef::F32(buf) => self.copy_interleaved_typed(&buf),
            AudioBufferRef::S32(buf) => self.copy_interleaved_typed(&buf),
        }
    }

    /// Copies all audio data from a source `AudioBuffer` that is of a different sample format type
    /// than that of the `RawSampleBuffer` in interleaved channel order. The two buffers must be
    /// equivalent.
    pub fn copy_interleaved_typed<F>(&mut self, src: &AudioBuffer<F>)
    where
        F: Sample + IntoSample<S>,
    {
        let n_frames = src.n_frames;
        let n_channels = src.spec.channels.count();
        let n_samples = n_frames * n_channels;

        // Ensure that the capacity of the sample buffer is greater than or equal to the number
        // of samples that will be copied from the source buffer.
        assert!(self.capacity() >= n_samples);

        let mut writer = SampleWriter::from_buf(n_samples, self);

        // Provide slightly optimized interleave algorithms for Mono and Stereo buffers.
        match n_channels {
            // No channels, do nothing.
            0 => (),
            // Mono
            1=> {
                for m in &src.buf[0..n_frames] {
                    S::write((*m).into_sample(), &mut writer);
                }
            },
            // Stereo
            2 => {
                let l_buf = &src.buf[0..n_frames];
                let r_buf = &src.buf[src.n_capacity..(src.n_capacity + n_frames)];

                for (l, r) in l_buf.iter().zip(r_buf) {
                    S::write((*l).into_sample(), &mut writer);
                    S::write((*r).into_sample(), &mut writer);
                }
            },
            // 3+ channels
            _ => {
                let stride = src.n_capacity;

                for i in 0..n_frames {
                    //TODO: possibly replace by Slice::chunks() and Iterator::step_by()
                    for ch in 0..n_channels {
                        let s = src.buf[ch * stride + i];
                        S::write(s.into_sample(), &mut writer);
                    }
                }
            },
        }
    }

    /// Copies all audio data from the source `AudioBuffer` to the `RawSampleBuffer` in interleaved
    /// channel order. The two buffers must be equivalent.
    pub fn copy_interleaved(&mut self, src: &AudioBuffer<S>) {
        let n_frames = src.n_frames;
        let n_channels = src.spec.channels.count();
        let n_samples = n_frames * n_channels;

        // Ensure that the capacity of the sample buffer is greater than or equal to the number
        // of samples that will be copied from the source buffer.
        assert!(self.capacity() >= n_samples);

        let mut writer = SampleWriter::from_buf(n_samples, self);

        // Provide slightly optimized interleave algorithms for Mono and Stereo buffers.
        match n_channels {
            // No channels, do nothing.
            0 => (),
            // Mono
            1=> {
                for m in &src.buf[0..n_frames] {
                    S::write(*m, &mut writer);
                }
            },
            // Stereo
            2 => {
                let l_buf = &src.buf[0..n_frames];
                let r_buf = &src.buf[src.n_capacity..(src.n_capacity + n_frames)];

                for (l, r) in l_buf.iter().zip(r_buf) {
                    S::write(*l, &mut writer);
                    S::write(*r, &mut writer);
                }
            },
            // 3+ channels
            _ => {
                let stride = src.n_capacity;

                for i in 0..n_frames {
                    //TODO: possibly replace by Slice::chunks() and Iterator::step_by()
                    for ch in 0..n_channels {
                        S::write(src.buf[ch * stride + i], &mut writer);
                    }
                }
            },
        }
    }

    /// Gets a mutable byte buffer from the `RawSampleBuffer` where samples may be written. Calls to
    /// this function will overwrite any previously written data since it is not known how the
    /// samples for each channel are laid out in the buffer.
    fn req_bytes_mut(&mut self, n_samples: usize) -> &mut [u8] {
        assert!(n_samples <= self.capacity());

        let end = n_samples * mem::size_of::<S::StreamType>();
        self.n_written = n_samples;
        &mut self.buf[..end]
    }
}

/// A `SampleWriter` allows for the efficient writing of samples of a specific type to a
/// `RawSampleBuffer`. A `SampleWriter` can only be instantiated by a `StreamBuffer`.
///
/// While `SampleWriter` could simply be implemented as a byte stream writer with generic
/// write functions to support most use cases, this would be unsafe as it decouple's a
/// sample's StreamType, the data type used to allocate the `RawSampleBuffer`, from the amount
/// of data actually written to the `RawSampleBuffer` per Sample. Therefore, `SampleWriter` is
/// generic across the Sample trait and provides precisely one `write()` function that takes
/// exactly one reference to a Sample's StreamType. The result of this means that there will
/// never be an alignment issue, and the underlying byte vector can simply be converted to a
/// StreamType slice. This allows the compiler to use the most efficient method of copying
/// the encoded sample value to the underlying buffer.
pub struct SampleWriter<'a, S: Sample + WriteSample> {
    buf: &'a mut [S::StreamType],
    next: usize,
}

impl<'a, S: Sample + WriteSample> SampleWriter<'a, S> {

    fn from_buf(n_samples: usize, buf: &mut RawSampleBuffer<S>) -> SampleWriter<S> {
        let bytes = buf.req_bytes_mut(n_samples);
        //TODO: explain why this is safe
        unsafe {
            SampleWriter {
                buf: slice::from_raw_parts_mut(
                    bytes.as_mut_ptr() as *mut S::StreamType, buf.capacity()),
                next: 0,
            }
        }
    }

    pub fn write(&mut self, src: S::StreamType) {
        // Copy the source sample to the output buffer at the next writeable index.
        self.buf[self.next] = src;
        // Increment writeable index.
        self.next += 1;
    }

}
