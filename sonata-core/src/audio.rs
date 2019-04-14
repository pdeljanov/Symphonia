// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA


use std::fmt;
use std::vec::Vec;
use std::mem;
use std::slice;
use std::marker::PhantomData;

use bitflags::bitflags;

use super::errors::Result;
use super::sample::{Sample, i24, u24};

/// A `Timestamp` indicates an instantaneous moment in time.
#[derive(Copy, Clone)]
pub enum Timestamp {
    /// The time is expressed by a number of frames.
    Frame(u64),
    /// The time is expressed by a number of seconds.
    Time(f64),
}

/// A `Duration` indicates a span of time.
#[derive(Copy, Clone)]
pub enum Duration {
    /// The duration is expressed by an amount of frames.
    Frames(u64),
    /// The duration is expressed by an amount of time.
    Seconds(f64),
}

bitflags! {
    /// Channels is a bit mask of all channels contained in a signal.
    pub struct Channels: u32 {
        /// Front-left (left) or the Mono channel.
        const FRONT_LEFT         = 0x0000001;
        /// Front-right (right) channel.
        const FRONT_RIGHT        = 0x0000002;
        /// Front-centre (centre) channel.
        const FRONT_CENTRE       = 0x0000004;
        /// Rear-left (surround rear left) channel.
        const REAR_LEFT          = 0x0000008;
        /// Rear-centre (surround rear centre) channel.
        const REAR_CENTRE        = 0x0000010;
        /// Rear-right (surround rear right) channel.
        const REAR_RIGHT         = 0x0000020;
        /// Low frequency channel 1.
        const LFE1               = 0x0000040;
        /// Front left-of-centre (left center) channel.
        const FRONT_LEFT_CENTRE  = 0x0000080;
        /// Front right-of-centre (right center) channel.
        const FRONT_RIGHT_CENTRE = 0x0000100;
        /// Rear left-of-centre channel.
        const REAR_LEFT_CENTRE   = 0x0000200;
        /// Rear right-of-centre channel.
        const REAR_RIGHT_CENTRE  = 0x0000400;
        /// Front left-wide channel.
        const FRONT_LEFT_WIDE    = 0x0000800;
        /// Front right-wide channel.
        const FRONT_RIGHT_WIDE   = 0x0001000;
        /// Front left-high channel.
        const FRONT_LEFT_HIGH    = 0x0002000;
        /// Front centre-high channel.
        const FRONT_CENTRE_HIGH  = 0x0004000;
        /// Front right-high channel.
        const FRONT_RIGHT_HIGH   = 0x0008000;
        /// Low frequency channel 2.
        const LFE2               = 0x0010000;
        /// Side left (surround left) channel.
        const SIDE_LEFT          = 0x0020000;
        /// Side right (surround right) channel.
        const SIDE_RIGHT         = 0x0040000;
        /// Top centre channel.
        const TOP_CENTRE         = 0x0080000;
        /// Top front-left channel.
        const TOP_FRONT_LEFT     = 0x0100000;
        /// Top centre channel.
        const TOP_FRONT_CENTRE   = 0x0200000;
        /// Top front-right channel.
        const TOP_FRONT_RIGHT    = 0x0400000;
        /// Top rear-left channel.
        const TOP_REAR_LEFT      = 0x0800000;
        /// Top rear-centre channel.
        const TOP_REAR_CENTRE    = 0x1000000;
        /// Top rear-right channel.
        const TOP_REAR_RIGHT     = 0x2000000;
    }
}

impl Channels {
    /// Gets the number of channels.
    pub fn len(&self) -> usize {
        self.bits.count_ones() as usize
    }
}

impl fmt::Display for Channels {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#032b}", self.bits)
    }
}

/// Layout describes common audio channel configurations.
#[derive(Copy, Clone)]
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


/// SignalSpec describes the characteristics of a Signal.
#[derive(Copy, Clone)]
pub struct SignalSpec {
    /// The signal sampling rate in hertz (Hz).
    pub rate: u32,

    /// The channel assignments of the signal. The order of the channels in the vector is the order
    /// in which each channel sample is stored in a frame.
    pub channels: Channels
}

impl SignalSpec {
    pub fn new(rate: u32, channels: Channels) -> Self {
        SignalSpec {
            rate,
            channels,
        }
    }

    pub fn new_with_layout(rate: u32, layout: Layout) -> Self {
        SignalSpec {
            rate,
            channels: layout_to_channels(layout),
        }
    }
}

fn layout_to_channels(layout: Layout) -> Channels {
    match layout {
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


/// `WriteSample` provides a typed interface for converting a sample from it's in-memory type to it's 
/// StreamType.
pub trait WriteSample : Sample {
    fn write(sample: &Self, dest: &mut SampleWriter<Self>);
}

impl WriteSample for u8 {
    #[inline]
    fn write(sample: &u8, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for i8 {
    #[inline]
    fn write(sample: &i8, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for u16 {
    #[inline]
    fn write(sample: &u16, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for i16 {
    #[inline]
    fn write(sample: &i16, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for u24 {
    #[inline]
    fn write(sample: &u24, writer: &mut SampleWriter<Self>) {
        let bytes = [
            ((sample.0 & 0x0000ff) >>  0) as u8,
            ((sample.0 & 0x00ff00) >>  8) as u8,
            ((sample.0 & 0xff0000) >> 16) as u8,
        ];
        writer.write(&bytes);
    }
}

impl WriteSample for i24 {
    #[inline]
    fn write(sample: &i24, writer: &mut SampleWriter<Self>) {
        let bytes = [
            ((sample.0 & 0x0000ff) >>  0) as u8,
            ((sample.0 & 0x00ff00) >>  8) as u8,
            ((sample.0 & 0xff0000) >> 16) as u8,
        ];
        writer.write(&bytes);
    }
}

impl WriteSample for u32 {
    #[inline]
    fn write(sample: &u32, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for i32 {
    #[inline]
    fn write(sample: &i32, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for f32 {
    #[inline]
    fn write(sample: &f32, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

impl WriteSample for f64 {
    #[inline]
    fn write(sample: &f64, writer: &mut SampleWriter<Self>) {
        writer.write(sample);
    }
}

/// `AudioPlanes` provides immutable slices to each audio channel (plane) contained in a signal.
pub struct AudioPlanes<'a, S: 'a + Sample> {
    planes: [&'a [S]; 32],
    n_planes: usize
}

impl<'a, S : Sample> AudioPlanes<'a, S> {
    /// Gets all the audio planes.
    pub fn planes(&mut self) -> &[&'a [S]] {
        &self.planes[..self.n_planes]
    }
}

/// `AudioPlanesMut` provides mutable slices to each audio channel (plane) contained in a signal.
pub struct AudioPlanesMut<'a, S: 'a + Sample> {
    planes: [&'a mut [S]; 32],
    n_planes: usize
}

impl<'a, S : Sample> AudioPlanesMut<'a, S> {
    /// Gets all the audio planes.
    pub fn planes(&mut self) -> &mut [&'a mut [S]] {
        &mut self.planes[..self.n_planes]
    }
}

/// `AudioBuffer` is a container for planar audio data. An `AudioBuffer` can store upto capacity samples. It implements 
/// the Signal trait for reading and writing audio to the buffer itself.
pub struct AudioBuffer<S : Sample> {
    buf: Vec<S>,
    spec: SignalSpec,
    n_frames: usize,
    n_capacity: usize,
}

impl<S : Sample> AudioBuffer<S> {

    pub fn new(duration: Duration, spec: &SignalSpec) -> Self {
        let n_capacity = match duration {
            Duration::Frames(frames) => frames,
            Duration::Seconds(time) => (time * (1f64 / spec.rate as f64)) as u64,
        };

        let n_sample_capacity = n_capacity * spec.channels.len() as u64;

        // Practically speaking, it is not possible to allocate more than usize samples.
        debug_assert!(n_sample_capacity <= usize::max_value() as u64);

        // Allocate memory for the sample data, but do not zero the memory.
        let mut buf = Vec::with_capacity(n_sample_capacity as usize);
        unsafe { buf.set_len(n_sample_capacity as usize) };

        AudioBuffer {
            buf,
            spec: spec.clone(),
            n_frames: 0,
            n_capacity: n_capacity as usize,
        }
    }

    /// Gets the signal specification for the buffer.
    pub fn spec(&self) -> &SignalSpec {
        &self.spec
    }

    /// Gets the total capacity of the buffer. The capacity is the maximum number of frames a buffer can store.
    pub fn capacity(&self) -> usize {
        self.n_capacity
    }

    /// Gets immutable references to all audio planes (channels) within the buffer.
    /// 
    /// Note: This is not a cheap operation. It is advisable that this call is only used when operating on batches of 
    /// frames. Generally speaking, it is almost always better to use `chan()` to selectively choose the plane to read.
    pub fn planes<'a>(&'a self) -> AudioPlanes<'a, S> {
        let mut planes = AudioPlanes {
            planes: unsafe { std::mem::uninitialized() },
            n_planes: self.spec.channels.len(),
        };

        // Only fill the planes array up to the number of channels.
        for i in 0..planes.n_planes {
            let start = i * self.n_capacity;
            planes.planes[i] = &self.buf[start..start + self.n_frames];
        }

        planes
    }

    /// Gets mutable references to all audio planes (channels) within the buffer.
    /// 
    /// Note: This is not a cheap operation. It is advisable that this call is only used when mutating batches of 
    /// frames. Generally speaking, it is almost always better to use `render()`, `fill()`, `chan_mut()`, and 
    /// `chan_pair_mut()` to mutate the buffer.
    pub fn planes_mut<'a>(&'a mut self) -> AudioPlanesMut<'a, S> {
        let mut planes = AudioPlanesMut {
            planes: unsafe { std::mem::uninitialized() },
            n_planes: self.spec.channels.len(),
        };

        unsafe {
            let mut ptr = self.buf.as_mut_ptr();

            // Only fill the planes array up to the number of channels.
            for i in 0..planes.n_planes {
                planes.planes[i] = slice::from_raw_parts_mut(ptr as *mut S, self.n_frames);
                ptr = ptr.add(self.n_capacity);
            }
        }

        planes
    }

}

pub trait Signal<S : Sample> {
    /// Gets the number of actual frames written to the buffer. Conversely, this also is the number
    /// of written samples in any one channel.
    fn frames(&self) -> usize;

    /// Clears all written frames from the buffer. This is a cheap operation and does not zero the underlying audio 
    /// data.
    fn clear(&mut self);

    /// Gets an immutable reference to all the written samples in the specified channel.
    fn chan(&self, channel: u8) -> &[S];

    /// Gets a mutable reference to all the written samples in the specified channel.
    fn chan_mut(&mut self, channel: u8) -> &mut [S];

    /// Gets two mutable references to two different channels.
    fn chan_pair_mut(&mut self, first: u8, second: u8) -> (&mut [S], &mut [S]);

    /// Renders a reserved number of frames. This is a cheap operation and simply advances the frame counter. The 
    /// underlying audio data is not modified and should be overwritten through other means.
    /// 
    /// If `n_frames` is `None`, the remaining number of samples will be used. If `n_frames` is too large, this function
    /// will assert.
    fn render_reserved(&mut self, n_frames: Option<usize>);

    /// Renders a number of frames using the provided render function. The number of frames to render is specified by 
    /// `n_frames`. If `n_frames` is `None`, the remaining number of frames in the buffer will be rendered. If the 
    /// render function returns an error, the render operation is terminated prematurely.
    fn render<'a, F>(&'a mut self, n_frames: Option<usize>, render: F) -> Result<()>
    where
        F: FnMut(&mut AudioPlanesMut<'a, S>, usize) -> Result<()>;

    /// Clears, and then renders the entire buffer using the fill function. This is a convenience wrapper around `render`
    /// and exhibits the same behaviour as `render` in regards to the fill function.
    #[inline]
    fn fill<'a, F>(&'a mut self, fill: F) -> Result<()> 
    where
        F: FnMut(&mut AudioPlanesMut<'a, S>, usize) -> Result<()>
    {
        self.clear();
        self.render(None, fill)
    }

    /// Transforms every written sample in the signal using the transformation function provided. This function does not
    /// guarantee an order in which the samples are transformed.
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

    fn chan(&self, channel: u8) -> &[S]{
        let start = channel as usize * self.n_capacity;
        &self.buf[start..start + self.n_frames]
    }

    #[inline]
    fn chan_mut(&mut self, channel: u8) -> &mut [S] {
        let start = channel as usize * self.n_capacity;
        &mut self.buf[start..start + self.n_frames]
    }

    fn chan_pair_mut(&mut self, first: u8, second: u8) -> (&mut [S], &mut [S]) {
        let first_idx = self.n_capacity * first as usize;
        let second_idx = self.n_capacity * second as usize;

        assert!(first_idx < self.buf.len());
        assert!(second_idx <self.buf.len());

        unsafe {
            let ptr = self.buf.as_mut_ptr();
            (slice::from_raw_parts_mut(ptr.add(first_idx), self.n_frames),
             slice::from_raw_parts_mut(ptr.add(second_idx), self.n_frames))
        }
    }

    #[inline]
    fn render_reserved(&mut self, n_frames: Option<usize>) {
        let n_reserved_frames = n_frames.unwrap_or(self.n_capacity - self.n_frames);
        assert!(self.n_frames + n_reserved_frames <= self.n_capacity);
        self.n_frames += n_reserved_frames;
    }

    fn render<'a, F>(&'a mut self, n_frames: Option<usize>, mut render: F) -> Result<()>
    where
        F: FnMut(&mut AudioPlanesMut<'a, S>, usize) -> Result<()> 
    {
        // Calculate the number of frames to render if it is not provided.
        let n_render_frames = n_frames.unwrap_or(self.n_capacity - self.n_frames);
        let end = self.n_frames + n_render_frames;

        assert!(end <= self.n_capacity);

        let mut planes = AudioPlanesMut {
            planes: unsafe { std::mem::uninitialized() },
            n_planes: self.spec.channels.len(),
        };

        unsafe {
            let mut ptr = self.buf.as_mut_ptr().add(self.n_frames);

            // Only fill the planes array up to the number of channels.
            for i in 0..planes.n_planes {
                planes.planes[i] = slice::from_raw_parts_mut(ptr as *mut S, n_render_frames);
                ptr = ptr.add(self.n_capacity);
            }
        }
        
        // Attempt to fill the entire buffer, exiting only if there is an error.
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

        unsafe {
            let mut plane_start = self.buf.as_mut_ptr();
            let buffer_end = plane_start.add(self.buf.len());
            
            while plane_start < buffer_end {
                let plane_end = plane_start.add(self.n_frames);

                let mut ptr = plane_start;
                while ptr < plane_end {
                    *ptr = f(*ptr);
                    ptr = ptr.add(1);
                }

                plane_start = plane_start.add(self.n_capacity);
            }
        }
    }

}


/// A `SampleBuffer`, as the name implies, is a Sample oriented buffer. It is agnostic to the ordering/layout 
/// of samples within the buffer. 
pub struct SampleBuffer<S: Sample> {
    buf: Vec<u8>,
    n_written: usize,
    // Might take your heart.
    sample_format: PhantomData<S>,
}

impl<S: Sample> SampleBuffer<S> {

    pub fn new(duration: Duration, spec: &SignalSpec) -> SampleBuffer<S> {
        let n_frames = match duration {
            Duration::Frames(frames) => frames,
            Duration::Seconds(time) => (time * (1f64 / spec.rate as f64)) as u64,
        };

        let n_samples = n_frames * spec.channels.len() as u64;

        // Practically speaking, it is not possible to allocate more than usize samples.
        debug_assert!(n_samples <= usize::max_value() as u64);

        // Allocate enough memory for all the samples.
        let byte_length = n_samples as usize * mem::size_of::<S::StreamType>();
        let mut buf = Vec::with_capacity(byte_length);
        unsafe { buf.set_len(byte_length) };

        SampleBuffer {
            buf,
            n_written: 0,
            sample_format: PhantomData,
        }
    }

    /// Gets the amount of valid (written) samples stored.
    pub fn samples(&self) -> usize {
        self.n_written
    }

    /// Gets the maximum number of samples the `SampleBuffer` may store.
    pub fn capacity(&self) -> usize {
        self.buf.len() / mem::size_of::<S>()
    }

    /// Gets a mutable byte buffer from the `SampleBuffer` where samples may be written. Calls to this function will 
    /// overwrite any previously written data since it is not known how the samples for each channel are laid out in
    /// the buffer.
    pub fn req_bytes_mut(&mut self, n_samples: usize) -> &mut [u8] {
        assert!(n_samples <= self.capacity());

        let end = n_samples * mem::size_of::<S::StreamType>();
        self.n_written = n_samples;
        &mut self.buf[..end]
    }

    /// Gets an immutable slice to the bytes of the sample's written in the `SampleBuffer`.
    pub fn as_bytes(&self) -> &[u8] {
        let end = self.n_written * mem::size_of::<S::StreamType>();
        &self.buf[..end]
    }

}


/// A `SampleWriter` allows for the efficient writing of samples of a specific type to a 
/// `SampleBuffer`. A `SampleWriter` can only be instantiated by a `StreamBuffer`.
/// 
/// While `SampleWriter` could simply be implemented as a byte stream writer with generic 
/// write functions to support most use cases, this would be unsafe as it decouple's a 
/// sample's StreamType, the data type used to allocate the `SampleBuffer`, from the amount 
/// of data actually written to the `SampleBuffer` per Sample. Therefore, `SampleWriter` is 
/// generic across the Sample trait and provides precisely one `write()` function that takes
/// exactly one reference to a Sample's StreamType. The result of this means that there will
/// never be an alignment issue, and the underlying byte vector can simply be converted to a
/// StreamType slice. This allows the compiler to use the most efficient method of copying 
/// the encoded sample value to the underlying buffer.
pub struct SampleWriter<'a, S: Sample> {
    buf: &'a mut [S::StreamType],
    next: usize,
}

impl<'a, S: Sample> SampleWriter<'a, S> {

    fn from_buf(n_samples: usize, buf: &mut SampleBuffer<S>) -> SampleWriter<S> {
        let bytes = buf.req_bytes_mut(n_samples);
        unsafe {
            SampleWriter {
                buf: slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut S::StreamType, buf.capacity()),
                next: 0,
            }
        }
    }

    pub fn write(&mut self, src: &S::StreamType) {
        // Copy the source sample to the output buffer at the next writeable index.
        unsafe { *self.buf.get_unchecked_mut(self.next) = *src; }
        // Increment writeable index.
        self.next += 1;
    }

}

/// `ExportBuffer` provides the interface to copy the contents of a buffer containing 
/// audio samples into a `SampleBuffer`. When exported, Sample's that have a StreamType 
/// that is not the same as it's in-memory type will be encoded as the StreamType first. 
/// If the implementor of `ExportBuffer` over-provisions samples, only the actual samples
/// in the source will be exported.
pub trait ExportBuffer<S: Sample + WriteSample> {
    /// Copies all samples from a channel to the `SampleBuffer` before copying the 
    /// next channel.
    fn copy_planar(&self, dst: &mut SampleBuffer<S>);

    /// Copies one sample per channel as a set to the `SampleBuffer` before copying
    /// the next set.
    fn copy_interleaved(&self, dst: &mut SampleBuffer<S>);
}

impl<S: Sample + WriteSample> ExportBuffer<S> for AudioBuffer<S> {

    fn copy_planar(&self, dst: &mut SampleBuffer<S>) {
        let n_frames = self.n_frames;
        let n_channels = self.spec.channels.len();
        let n_samples = n_frames * n_channels;

        // Ensure that the capacity of the destination buffer is greater than or equal to the number of 
        // samples that will be copied.
        assert!(dst.capacity() >= n_samples);

        let mut writer = SampleWriter::from_buf(n_samples, dst);

        // Provide slightly optimized copy algorithms for Mono and Stereo buffers.
        match n_channels {
            // No channels, do nothing.
            0 => (),
            // Mono
            1 => {
                for i in 0..n_frames {
                    unsafe { S::write(self.buf.get_unchecked(i), &mut writer); }
                }
            },
            // Stereo
            2 => {
                for i in 0..n_frames {
                    unsafe { S::write(self.buf.get_unchecked(i), &mut writer); }
                }
                for i in self.n_capacity..self.n_capacity + n_frames {
                    unsafe { S::write(self.buf.get_unchecked(i), &mut writer); }
                }
            },
            // 3+ channels
            _ => {
                let mut k = 0;
                for _ in 0..n_channels {
                    for i in 0..n_frames {
                        unsafe { S::write(self.buf.get_unchecked(k + i), &mut writer); }
                    }
                    // Advance the start index for the next channel by the source buffer stride.
                    k += self.n_capacity;
                }
            }
        }

    }

    fn copy_interleaved(&self, dst: &mut SampleBuffer<S>) {
        let n_frames = self.n_frames;
        let n_channels = self.spec.channels.len();
        let n_samples = n_frames * n_channels;

        let mut writer = SampleWriter::from_buf(n_samples, dst);

        let stride = self.n_capacity;

        // Provide slightly optimized interleave algorithms for Mono and Stereo buffers.
        match n_channels {
            // No channels, do nothing.
            0 => (),
            // Mono
            1=> {
                for i in 0..n_frames {
                    unsafe { S::write(self.buf.get_unchecked(i), &mut writer); }
                }
            },
            // Stereo
            2 => {
                for i in 0..n_frames {
                    unsafe { 
                        S::write(self.buf.get_unchecked(i), &mut writer); 
                        S::write(self.buf.get_unchecked(i + stride), &mut writer);
                    }
                }
            },
            // 3+ channels
            _ => {
                for i in 0..n_frames {
                    for j in 0..n_channels {
                        unsafe { S::write(self.buf.get_unchecked(j*stride + i), &mut writer); }
                    }
                }
            }
        }
    }

}

