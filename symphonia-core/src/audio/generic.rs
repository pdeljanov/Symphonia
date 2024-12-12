// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::audio::{
    conv::ConvertibleSample,
    sample::{i24, u24, SampleBytes, SampleFormat},
};

use super::{Audio, AudioBuffer, AudioBytes, AudioMut, AudioSpec};

/// A container for an owned [`AudioBuffer`] of any standard sample format.
pub enum GenericAudioBuffer {
    /// An unsigned 8-bit integer buffer.
    U8(AudioBuffer<u8>),
    /// An unsigned 16-bit integer buffer.
    U16(AudioBuffer<u16>),
    /// An unsigned 24-bit integer buffer.
    U24(AudioBuffer<u24>),
    /// An unsigned 32-bit integer buffer.
    U32(AudioBuffer<u32>),
    /// A signed 8-bit integer buffer.
    S8(AudioBuffer<i8>),
    /// A signed 16-bit integer buffer.
    S16(AudioBuffer<i16>),
    /// A signed 24-bit integer buffer.
    S24(AudioBuffer<i24>),
    /// A signed 32-bit integer buffer.
    S32(AudioBuffer<i32>),
    /// A single precision (32-bit) floating point buffer.
    F32(AudioBuffer<f32>),
    /// A double precision (64-bit) floating point buffer.
    F64(AudioBuffer<f64>),
}

macro_rules! impl_generic_func {
    ($own:expr, $buf:ident, $expr:expr) => {
        match $own {
            GenericAudioBuffer::U8($buf) => $expr,
            GenericAudioBuffer::U16($buf) => $expr,
            GenericAudioBuffer::U24($buf) => $expr,
            GenericAudioBuffer::U32($buf) => $expr,
            GenericAudioBuffer::S8($buf) => $expr,
            GenericAudioBuffer::S16($buf) => $expr,
            GenericAudioBuffer::S24($buf) => $expr,
            GenericAudioBuffer::S32($buf) => $expr,
            GenericAudioBuffer::F32($buf) => $expr,
            GenericAudioBuffer::F64($buf) => $expr,
        }
    };
}

impl GenericAudioBuffer {
    pub fn new(format: SampleFormat, spec: AudioSpec, capacity: usize) -> Self {
        match format {
            SampleFormat::U8 => GenericAudioBuffer::U8(AudioBuffer::new(spec, capacity)),
            SampleFormat::U16 => GenericAudioBuffer::U16(AudioBuffer::new(spec, capacity)),
            SampleFormat::U24 => GenericAudioBuffer::U24(AudioBuffer::new(spec, capacity)),
            SampleFormat::U32 => GenericAudioBuffer::U32(AudioBuffer::new(spec, capacity)),
            SampleFormat::S8 => GenericAudioBuffer::S8(AudioBuffer::new(spec, capacity)),
            SampleFormat::S16 => GenericAudioBuffer::S16(AudioBuffer::new(spec, capacity)),
            SampleFormat::S24 => GenericAudioBuffer::S24(AudioBuffer::new(spec, capacity)),
            SampleFormat::S32 => GenericAudioBuffer::S32(AudioBuffer::new(spec, capacity)),
            SampleFormat::F32 => GenericAudioBuffer::F32(AudioBuffer::new(spec, capacity)),
            SampleFormat::F64 => GenericAudioBuffer::F64(AudioBuffer::new(spec, capacity)),
        }
    }

    /// Get the audio specification.
    pub fn spec(&self) -> &AudioSpec {
        impl_generic_func!(self, buf, buf.spec())
    }

    /// Get the total number of audio planes.
    pub fn num_planes(&self) -> usize {
        impl_generic_func!(self, buf, buf.num_planes())
    }

    /// Returns `true` if there are no audio frames.
    pub fn is_empty(&self) -> bool {
        impl_generic_func!(self, buf, buf.is_empty())
    }

    /// Gets the number of audio frames in the buffer.
    pub fn frames(&self) -> usize {
        impl_generic_func!(self, buf, buf.frames())
    }

    /// Returns `true` if the `AudioBuffer` is unused.
    ///
    /// An unused `AudioBuffer` has either a capacity of 0, or no channels.
    pub fn is_unused(&self) -> bool {
        impl_generic_func!(self, buf, buf.is_unused())
    }

    /// Gets the total capacity of the buffer. The capacity is the maximum number of audio frames
    /// the buffer can store.
    pub fn capacity(&self) -> usize {
        impl_generic_func!(self, buf, buf.capacity())
    }

    /// Clears all audio frames.
    pub fn clear(&mut self) {
        impl_generic_func!(self, buf, buf.clear());
    }

    pub fn resize_with_silence(&mut self, new_len: usize) {
        impl_generic_func!(self, buf, buf.resize_with_silence(new_len))
    }

    pub fn resize_uninit(&mut self, new_len: usize) {
        impl_generic_func!(self, buf, buf.resize_uninit(new_len))
    }

    pub fn render_silence(&mut self, num_frames: Option<usize>) -> usize {
        impl_generic_func!(self, buf, buf.render_silence(num_frames))
    }

    pub fn render_uninit(&mut self, num_frames: Option<usize>) -> usize {
        impl_generic_func!(self, buf, buf.render_uninit(num_frames))
    }

    pub fn shift(&mut self, shift: usize) {
        impl_generic_func!(self, buf, buf.shift(shift))
    }

    pub fn truncate(&mut self, num_frames: usize) {
        impl_generic_func!(self, buf, buf.truncate(num_frames))
    }

    pub fn trim(&mut self, start: usize, end: usize) {
        impl_generic_func!(self, buf, buf.trim(start, end))
    }

    /// Get the total number of samples contained in all audio planes.
    pub fn samples_interleaved(&self) -> usize {
        self.num_planes() * self.frames()
    }

    /// Get the total number of samples contained in each audio plane.
    pub fn samples_planar(&self) -> usize {
        self.frames()
    }

    pub fn copy_to<Sout, Dst>(&self, dst: &mut Dst)
    where
        Sout: ConvertibleSample,
        Dst: AudioMut<Sout>,
    {
        impl_generic_func!(self, buf, dst.copy_from(buf));
    }

    pub fn copy_to_slice_interleaved<Sout, Dst>(&self, dst: Dst)
    where
        Sout: ConvertibleSample,
        Dst: AsMut<[Sout]>,
    {
        impl_generic_func!(self, buf, buf.copy_to_slice_interleaved(dst))
    }

    pub fn copy_to_slice_planar<Sout, Dst>(&self, dst: &mut [Dst])
    where
        Sout: ConvertibleSample,
        Dst: AsMut<[Sout]>,
    {
        impl_generic_func!(self, buf, buf.copy_to_slice_planar(dst))
    }

    pub fn copy_to_vec_interleaved<Sout>(&self, dst: &mut Vec<Sout>)
    where
        Sout: ConvertibleSample,
    {
        impl_generic_func!(self, buf, buf.copy_to_vec_interleaved(dst))
    }

    pub fn copy_to_vecs_planar<Sout>(&self, dst: &mut Vec<Vec<Sout>>)
    where
        Sout: ConvertibleSample,
    {
        impl_generic_func!(self, buf, buf.copy_to_vecs_planar(dst))
    }

    pub fn copy_bytes_interleaved_as<Sout, Dst>(&self, dst: Dst)
    where
        Sout: SampleBytes + ConvertibleSample,
        Dst: AsMut<[u8]>,
    {
        impl_generic_func!(self, buf, buf.copy_bytes_interleaved_as::<Sout, _>(dst))
    }

    pub fn copy_bytes_planar_as<Sout, Dst>(&self, dst: &mut [Dst])
    where
        Sout: SampleBytes + ConvertibleSample,
        Dst: AsMut<[u8]>,
    {
        impl_generic_func!(self, buf, buf.copy_bytes_planar_as::<Sout, _>(dst))
    }

    pub fn copy_bytes_interleaved<Sout, Dst>(&self, dst: Dst)
    where
        Sout: SampleBytes,
        Dst: AsMut<[u8]>,
    {
        impl_generic_func!(self, buf, buf.copy_bytes_interleaved(dst))
    }

    pub fn copy_bytes_planar<Sout, Dst>(&self, dst: &mut [Dst])
    where
        Sout: SampleBytes,
        Dst: AsMut<[u8]>,
    {
        impl_generic_func!(self, buf, buf.copy_bytes_planar(dst))
    }

    pub fn byte_len_as<Sout>(&self) -> usize
    where
        Sout: SampleBytes + ConvertibleSample,
    {
        impl_generic_func!(self, buf, buf.byte_len_as::<Sout>())
    }

    pub fn byte_len_per_plane_as<Sout>(&self) -> usize
    where
        Sout: SampleBytes + ConvertibleSample,
    {
        impl_generic_func!(self, buf, buf.byte_len_per_plane_as::<Sout>())
    }

    pub fn byte_len_per_frame_as<Sout>(&self) -> usize
    where
        Sout: SampleBytes + ConvertibleSample,
    {
        impl_generic_func!(self, buf, buf.byte_len_per_frame_as::<Sout>())
    }

    pub fn byte_len(&self) -> usize {
        impl_generic_func!(self, buf, buf.byte_len())
    }

    pub fn byte_len_per_plane(&self) -> usize {
        impl_generic_func!(self, buf, buf.byte_len_per_plane())
    }

    pub fn byte_len_per_frame(&self) -> usize {
        impl_generic_func!(self, buf, buf.byte_len_per_frame())
    }
}

/// A immutable reference to an [`AudioBuffer`] of any standard sample format.
#[derive(Clone)]
pub enum GenericAudioBufferRef<'a> {
    /// An unsigned 8-bit integer buffer reference.
    U8(&'a AudioBuffer<u8>),
    /// An unsigned 16-bit integer buffer reference.
    U16(&'a AudioBuffer<u16>),
    /// An unsigned 24-bit integer buffer reference.
    U24(&'a AudioBuffer<u24>),
    /// An unsigned 32-bit integer buffer reference.
    U32(&'a AudioBuffer<u32>),
    /// A signed 8-bit integer buffer reference.
    S8(&'a AudioBuffer<i8>),
    /// A signed 16-bit integer buffer reference.
    S16(&'a AudioBuffer<i16>),
    /// A signed 24-bit integer buffer reference.
    S24(&'a AudioBuffer<i24>),
    /// A signed 32-bit integer buffer reference.
    S32(&'a AudioBuffer<i32>),
    /// A single precision (32-bit) floating point buffer reference.
    F32(&'a AudioBuffer<f32>),
    /// A double precision (64-bit) floating point buffer reference.
    F64(&'a AudioBuffer<f64>),
}

macro_rules! impl_generic_ref_func {
    ($var:expr, $buf:ident,$expr:expr) => {
        match $var {
            GenericAudioBufferRef::U8($buf) => $expr,
            GenericAudioBufferRef::U16($buf) => $expr,
            GenericAudioBufferRef::U24($buf) => $expr,
            GenericAudioBufferRef::U32($buf) => $expr,
            GenericAudioBufferRef::S8($buf) => $expr,
            GenericAudioBufferRef::S16($buf) => $expr,
            GenericAudioBufferRef::S24($buf) => $expr,
            GenericAudioBufferRef::S32($buf) => $expr,
            GenericAudioBufferRef::F32($buf) => $expr,
            GenericAudioBufferRef::F64($buf) => $expr,
        }
    };
}

impl GenericAudioBufferRef<'_> {
    /// Get the audio specification.
    pub fn spec(&self) -> &AudioSpec {
        impl_generic_ref_func!(self, buf, buf.spec())
    }

    /// Get the total number of audio planes.
    pub fn num_planes(&self) -> usize {
        impl_generic_ref_func!(self, buf, buf.num_planes())
    }

    /// Returns `true` if there are no audio frames.
    pub fn is_empty(&self) -> bool {
        impl_generic_ref_func!(self, buf, buf.is_empty())
    }

    /// Gets the number of audio frames in the buffer.
    pub fn frames(&self) -> usize {
        impl_generic_ref_func!(self, buf, buf.frames())
    }

    /// Returns `true` if the referenced `AudioBuffer` is unused.
    ///
    /// An unused `AudioBuffer` has either a capacity of 0, or no channels.
    pub fn is_unused(&self) -> bool {
        impl_generic_ref_func!(self, buf, buf.is_unused())
    }

    /// Gets the total capacity of the buffer. The capacity is the maximum number of audio frames
    /// the buffer can store.
    pub fn capacity(&self) -> usize {
        impl_generic_ref_func!(self, buf, buf.capacity())
    }

    /// Get the total number of samples contained in all audio planes.
    pub fn samples_interleaved(&self) -> usize {
        self.num_planes() * self.frames()
    }

    /// Get the total number of samples contained in each audio plane.
    pub fn samples_planar(&self) -> usize {
        self.frames()
    }

    pub fn copy_to<Sout, Dst>(&self, dst: &mut Dst)
    where
        Sout: ConvertibleSample,
        Dst: AudioMut<Sout>,
    {
        impl_generic_ref_func!(self, buf, dst.copy_from(*buf));
    }

    pub fn copy_to_slice_interleaved<Sout, Dst>(&self, dst: Dst)
    where
        Sout: ConvertibleSample,
        Dst: AsMut<[Sout]>,
    {
        impl_generic_ref_func!(self, buf, buf.copy_to_slice_interleaved(dst))
    }

    pub fn copy_to_slice_planar<Sout, Dst>(&self, dst: &mut [Dst])
    where
        Sout: ConvertibleSample,
        Dst: AsMut<[Sout]>,
    {
        impl_generic_ref_func!(self, buf, buf.copy_to_slice_planar(dst))
    }

    pub fn copy_to_vec_interleaved<Sout>(&self, dst: &mut Vec<Sout>)
    where
        Sout: ConvertibleSample,
    {
        impl_generic_ref_func!(self, buf, buf.copy_to_vec_interleaved(dst))
    }

    pub fn copy_to_vecs_planar<Sout>(&self, dst: &mut Vec<Vec<Sout>>)
    where
        Sout: ConvertibleSample,
    {
        impl_generic_ref_func!(self, buf, buf.copy_to_vecs_planar(dst))
    }

    pub fn copy_bytes_interleaved_as<Sout, Dst>(&self, dst: Dst)
    where
        Sout: SampleBytes + ConvertibleSample,
        Dst: AsMut<[u8]>,
    {
        impl_generic_ref_func!(self, buf, buf.copy_bytes_interleaved_as::<Sout, _>(dst))
    }

    pub fn copy_bytes_planar_as<Sout, Dst>(&self, dst: &mut [Dst])
    where
        Sout: SampleBytes + ConvertibleSample,
        Dst: AsMut<[u8]>,
    {
        impl_generic_ref_func!(self, buf, buf.copy_bytes_planar_as::<Sout, _>(dst))
    }

    pub fn copy_bytes_interleaved<Sout, Dst>(&self, dst: Dst)
    where
        Sout: SampleBytes,
        Dst: AsMut<[u8]>,
    {
        impl_generic_ref_func!(self, buf, buf.copy_bytes_interleaved(dst))
    }

    pub fn copy_bytes_planar<Sout, Dst>(&self, dst: &mut [Dst])
    where
        Sout: SampleBytes,
        Dst: AsMut<[u8]>,
    {
        impl_generic_ref_func!(self, buf, buf.copy_bytes_planar(dst))
    }

    pub fn byte_len_as<Sout>(&self) -> usize
    where
        Sout: SampleBytes + ConvertibleSample,
    {
        impl_generic_ref_func!(self, buf, buf.byte_len_as::<Sout>())
    }

    pub fn byte_len_per_plane_as<Sout>(&self) -> usize
    where
        Sout: SampleBytes + ConvertibleSample,
    {
        impl_generic_ref_func!(self, buf, buf.byte_len_per_plane_as::<Sout>())
    }

    pub fn byte_len_per_frame_as<Sout>(&self) -> usize
    where
        Sout: SampleBytes + ConvertibleSample,
    {
        impl_generic_ref_func!(self, buf, buf.byte_len_per_frame_as::<Sout>())
    }

    pub fn byte_len(&self) -> usize {
        impl_generic_ref_func!(self, buf, buf.byte_len())
    }

    pub fn byte_len_per_plane(&self) -> usize {
        impl_generic_ref_func!(self, buf, buf.byte_len_per_plane())
    }

    pub fn byte_len_per_frame(&self) -> usize {
        impl_generic_ref_func!(self, buf, buf.byte_len_per_frame())
    }
}

/// A trait for generically borrowing an [`AudioBuffer`] by wrapping it in a
/// [`GenericAudioBufferRef`].
pub trait AsGenericAudioBufferRef {
    /// Get an immutable reference to the audio buffer as a generic audio buffer reference.
    fn as_generic_audio_buffer_ref(&self) -> GenericAudioBufferRef<'_>;
}

impl AsGenericAudioBufferRef for GenericAudioBuffer {
    fn as_generic_audio_buffer_ref(&self) -> GenericAudioBufferRef<'_> {
        impl_generic_func!(self, buf, buf.as_generic_audio_buffer_ref())
    }
}

// Implement AsicGenericAudioBufferRef for all AudioBuffers of standard sample formats.
macro_rules! impl_as_generic_audio_buffer_ref {
    ($fmt:ty, $ref:path) => {
        impl AsGenericAudioBufferRef for AudioBuffer<$fmt> {
            fn as_generic_audio_buffer_ref(&self) -> GenericAudioBufferRef<'_> {
                $ref(self)
            }
        }
    };
}

impl_as_generic_audio_buffer_ref!(u8, GenericAudioBufferRef::U8);
impl_as_generic_audio_buffer_ref!(u16, GenericAudioBufferRef::U16);
impl_as_generic_audio_buffer_ref!(u24, GenericAudioBufferRef::U24);
impl_as_generic_audio_buffer_ref!(u32, GenericAudioBufferRef::U32);
impl_as_generic_audio_buffer_ref!(i8, GenericAudioBufferRef::S8);
impl_as_generic_audio_buffer_ref!(i16, GenericAudioBufferRef::S16);
impl_as_generic_audio_buffer_ref!(i24, GenericAudioBufferRef::S24);
impl_as_generic_audio_buffer_ref!(i32, GenericAudioBufferRef::S32);
impl_as_generic_audio_buffer_ref!(f32, GenericAudioBufferRef::F32);
impl_as_generic_audio_buffer_ref!(f64, GenericAudioBufferRef::F64);

// Implement From for conversions between AudioBuffer and GenericAudioBuffer{Ref} for all
// standard sample formats.
macro_rules! impl_from_converions {
    ($fmt:ty, $own:path, $ref:path) => {
        // Infalliable conversion from AudioBuffer<S> to GenericAudioBuffer.
        impl From<AudioBuffer<$fmt>> for GenericAudioBuffer {
            fn from(value: AudioBuffer<$fmt>) -> Self {
                $own(value)
            }
        }

        // Falliable conversion from GenericAudioBuffer to AudioBuffer<S>.
        impl TryFrom<GenericAudioBuffer> for AudioBuffer<$fmt> {
            type Error = ();

            fn try_from(value: GenericAudioBuffer) -> Result<Self, Self::Error> {
                match value {
                    $own(buffer) => Ok(buffer),
                    _ => Err(()),
                }
            }
        }

        // Infalliable conversion from &AudioBuffer<S> to GenericAudioBufferRef.
        impl<'a> From<&'a AudioBuffer<$fmt>> for GenericAudioBufferRef<'a> {
            fn from(value: &'a AudioBuffer<$fmt>) -> Self {
                $ref(value)
            }
        }

        // Falliable conversion from GenericAudioBufferRef to &AudioBuffer<S>.
        impl<'a> TryFrom<GenericAudioBufferRef<'a>> for &'a AudioBuffer<$fmt> {
            type Error = ();

            fn try_from(value: GenericAudioBufferRef<'a>) -> Result<Self, Self::Error> {
                match value {
                    $ref(buffer) => Ok(buffer),
                    _ => Err(()),
                }
            }
        }
    };
}

impl_from_converions!(u8, GenericAudioBuffer::U8, GenericAudioBufferRef::U8);
impl_from_converions!(u16, GenericAudioBuffer::U16, GenericAudioBufferRef::U16);
impl_from_converions!(u24, GenericAudioBuffer::U24, GenericAudioBufferRef::U24);
impl_from_converions!(u32, GenericAudioBuffer::U32, GenericAudioBufferRef::U32);
impl_from_converions!(i8, GenericAudioBuffer::S8, GenericAudioBufferRef::S8);
impl_from_converions!(i16, GenericAudioBuffer::S16, GenericAudioBufferRef::S16);
impl_from_converions!(i24, GenericAudioBuffer::S24, GenericAudioBufferRef::S24);
impl_from_converions!(i32, GenericAudioBuffer::S32, GenericAudioBufferRef::S32);
impl_from_converions!(f32, GenericAudioBuffer::F32, GenericAudioBufferRef::F32);
impl_from_converions!(f64, GenericAudioBuffer::F64, GenericAudioBufferRef::F64);
