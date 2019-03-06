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

use std::vec::Vec;
use std::mem;
use std::slice;
use std::marker::PhantomData;

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

/// Channel defines the audio channel position.
#[derive(Debug, Copy, Clone)]
pub enum Channel {
    /// Mono, single channel.
    Mono,

    /// Front-left (left) channel.
    FrontLeft,
    /// Front-right (right) channel.
    FrontRight,
    /// Front-centre (centre) channel.
    FrontCentre,

    /// Rear-left (surround rear left) channel.
    RearLeft,
    /// Rear-centre (surround rear centre) channel.
    RearCentre,
    /// Rear-right (surround rear right) channel.
    RearRight,

    /// Low frequency channel 1.
    LFE1,

    /// Front left-of-centre (left center) channel.
    FrontLeftCentre,
    /// Front right-of-centre (right center) channel.
    FrontRightCentre,
    /// Rear left-of-centre channel.
    RearLeftCentre,

    /// Rear right-of-centre channel.
    RearRightCentre,
    /// Front left-wide channel.
    FrontLeftWide,
    /// Front right-wide channel.
    FrontRightWide,

    /// Front left-high channel.
    FrontLeftHigh,
    /// Front centre-high channel.
    FrontCentreHigh,
    /// Front right-high channel.
    FrontRightHigh,

    /// Low frequency channel 2.
    LFE2,

    /// Side left (surround left) channel.
    SideLeft,
    /// Side right (surround right) channel.
    SideRight,

    /// Top centre channel.
    TopCentre,

    /// Top front-left channel.
    TopFrontLeft,
    /// Top centre channel.
    TopFrontCentre,
    /// Top front-right channel.
    TopFrontRight,

    /// Top rear-left channel.
    TopRearLeft,
    /// Top rear-centre channel.
    TopRearCentre,
    /// Top rear-right channel.
    TopRearRight
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
#[derive(Clone)]
pub struct SignalSpec {
    /// The signal sampling rate in hertz (Hz).
    pub rate: u32,

    /// The channel assignments of the signal. The order of the channels in the vector is the order
    /// in which each channel sample is stored in a frame.
    pub channels: Vec<Channel>
}

impl SignalSpec {
    pub fn new(rate: u32, channels: &[Channel]) -> Self {
        SignalSpec {
            rate,
            channels: channels.to_vec(),
        }
    }

    pub fn new_with_layout(rate: u32, layout: Layout) -> Self {
        SignalSpec {
            rate,
            channels: layout_to_channels(layout),
        }
    }
}

fn layout_to_channels(layout: Layout) -> Vec<Channel> {
    match layout {
        Layout::Mono         => vec![Channel::Mono],
        Layout::Stereo       => vec![Channel::FrontLeft, Channel::FrontRight],
        Layout::TwoPointOne  => vec![Channel::FrontLeft, Channel::FrontRight, Channel::LFE1],
        Layout::FivePointOne => {
            vec![Channel::FrontLeft,
            Channel::FrontRight,
            Channel::FrontCentre,
            Channel::RearLeft,
            Channel::RearRight,
            Channel::LFE1]
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



pub trait Signal<S : Sample> {

    /// Gets the total capacity of the buffer. The capacity is the maximum number of frames a
    /// buffer can hold.
    fn capacity(&self) -> usize;

    /// Gets the number of actual frames written to the buffer. Conversely, this also is the number
    /// of written samples in any one channel.
    fn frames(&self) -> usize;

    /// Resets the number of frames to 0 allowing the buffer to be reused.
    fn renew(&mut self);

    /// Reserves `amount` number of frames for writing. This function will panic if the number of
    /// frames already written plus `amount` exceed the capacity.
    fn produce(&mut self, amount: usize);

    /// Gets an immutable reference to all the written samples in the specified channel.
    fn chan(&self, channel: u8) -> &[S];

    /// Gets a mutable reference to all the written samples in the specified channel.
    fn chan_mut(&mut self, channel: u8) -> &mut [S];

    /// Gets two mutable references to two different channels.
    fn chan_pair_mut(&mut self, first: u8, second: u8) -> (&mut [S], &mut [S]);
}

pub struct AudioBuffer<S : Sample + WriteSample> {
    buf: Vec<S>,
    spec: SignalSpec,
    n_frames: usize,
    n_capacity: usize,
}

impl<S : Sample + WriteSample> AudioBuffer<S> {

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

    pub fn as_planar(&self) -> &[S] {
        &self.buf[..self.n_frames * self.spec.channels.len()]
    }

    pub fn as_planar_mut(&mut self) -> &mut [S] {
        &mut self.buf[..self.n_frames * self.spec.channels.len()]
    }
    
}

impl<S : Sample + WriteSample> Signal<S> for AudioBuffer<S>{

    fn capacity(&self) -> usize {
        self.n_capacity
    }

    fn frames(&self) -> usize {
        self.n_frames
    }

    fn produce(&mut self, amount: usize) {
        self.n_frames += amount;
        assert!(self.n_frames <= self.n_capacity);
    }

    fn renew(&mut self) {
        self.n_frames = 0;
    }

    fn chan(&self, channel: u8) -> &[S]{
        let start = channel as usize * self.n_capacity;
        &self.buf[start..start + self.n_frames]
    }

    fn chan_mut(&mut self, channel: u8) -> &mut [S] {
        let start = channel as usize * self.n_capacity;
        &mut self.buf[start..start + self.n_frames]
    }

    fn chan_pair_mut(&mut self, first: u8, second: u8)
        -> (&mut [S], &mut [S]) {

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

    pub fn from_buf(n_samples: usize, buf: &mut SampleBuffer<S>) -> SampleWriter<S> {
        let bytes = buf.req_bytes_mut(n_samples);
        unsafe {
            SampleWriter {
                buf: slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut S::StreamType, buf.capacity()),
                next: 0,
            }
        }
    }

    pub fn write(&mut self, src: &S::StreamType) {
        //self.buf[self.next] = *src;
        unsafe {
            // Copy the source sample to the output buffer at the next writeable index.
            *self.buf.get_unchecked_mut(self.next) = *src;
        }
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
    /// 
    /// For example, for Stereo channels with 4 frames, the output buffer would 
    /// contain:
    /// 
    /// +---------------+
    /// |L|L|L|L|R|R|R|R|
    /// +---------------+
    /// 
    fn copy_planar(&self, dst: &mut SampleBuffer<S>);

    /// Copies one sample per channel as a set to the `SampleBuffer` before copying
    /// the next set.
    /// 
    /// For example, for Stereo channels with 4 frames, the output buffer would 
    /// contain: 
    /// 
    /// +---------------+
    /// |L|R|L|R|L|R|L|R|
    /// +---------------+
    /// 
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





