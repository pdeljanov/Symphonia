// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::vec::Vec;
use std::slice;
use std::mem;
use md5;
use sonata_core::audio::{AudioBuffer, Signal};

/// `Md5AudioValidator` computes the MD5 checksum of an audio stream taking into account the peculiarities of FLAC's
/// MD5 validation scheme.
pub struct Md5AudioValidator {
    state: md5::Context,
    hash_buf: Vec<u8>,
}

impl Md5AudioValidator {

    pub fn new() -> Self {
        Md5AudioValidator {
            state: md5::Context::new(),
            hash_buf: Vec::new(),
        }
    }

    pub fn update(&mut self, buf: &AudioBuffer<i32>, bps: u32) {
        // The MD5 checksum is calculated on a buffer containing interleaved audio samples of the correct sample width.
        // While FLAC can encode and decode samples of arbitrary bit widths, the samples in the buffer must be a 
        // multiple of 8-bits. 
        //
        // Additionally, Sonata's AudioBuffer's are in planar format, and the FLAC decoder works internally on signed 
        // 32-bit samples exclusively.
        //
        // Therefore, to compute the checksum, the audio buffer must be converted into an interlaced, and truncated 
        // hashing buffer. That hashing buffer can then be passed to the MD5 algorithm for hashing.

        // Round the sample bit width up to the nearest byte.
        let bytes_per_sample = match bps {
            0       => return,
            1..=8   => 1,
            9..=16  => 2,
            17..=24 => 3,
            25..=32 => 4,
            _ => unreachable!(),
        };

        let n_channels = buf.spec().channels.count();
        let n_frames = buf.frames();

        // Calculate the total size of all the samples in bytes.
        let byte_len = (n_channels * n_frames * bytes_per_sample) as usize;

        // Ensure the hash buffer length can accomodate all the samples.
        if self.hash_buf.len() < byte_len {
            self.hash_buf.resize(byte_len, 0u8);
        }

        // Populate the hash buffer with samples truncated to the correct width. A &[u8] slice of all the samples in 
        // hash buffer will be returned.
        let hash_slice = match bytes_per_sample {
            1 => slice_as_i8(&mut self.hash_buf, buf, n_channels, n_frames),
            2 => slice_as_i16(&mut self.hash_buf, buf, n_channels, n_frames),
            3 => slice_as_i24(&mut self.hash_buf, buf, n_channels, n_frames),
            4 => slice_as_i32(&mut self.hash_buf, buf, n_channels, n_frames),
            _ => unreachable!(),
        };

        // Update the MD5 state.
        self.state.consume(hash_slice);
    }

    pub fn finalize(&mut self) -> md5::Digest {
        self.state.clone().compute()
    }
}

fn slice_as_i24<'a>(hash_buf: &'a mut [u8], buf: &AudioBuffer<i32>, n_channels: usize, n_frames: usize) -> &'a [u8]{
    let frame_stride = 3 * n_channels;

    //TODO: explain why this is safe
    unsafe {
        for ch in 0..n_channels {
            let mut ptr = hash_buf.as_mut_ptr().add(3 * ch);

            for sample in buf.chan(ch as u8) {
                ptr.copy_from_nonoverlapping(sample.to_ne_bytes().as_ptr(), 3);
                ptr = ptr.add(frame_stride);
            }
        }
    }

    &hash_buf[..n_frames * frame_stride]
}

macro_rules! slice_as {
    ($name:ident, $type:ty) => {
        fn $name<'a>(hash_buf: &'a mut [u8], buf: &AudioBuffer<i32>, n_channels: usize, n_frames: usize) -> &'a [u8] {
            //TODO: explain why this is safe
            unsafe {
                let hash_slice = slice::from_raw_parts_mut(hash_buf.as_mut_ptr() as *mut $type, n_frames * n_channels);
                
                for ch in 0..n_channels {
                    let mut ptr = hash_slice.as_mut_ptr().add(ch);

                    for sample in buf.chan(ch as u8) {
                        *ptr = *sample as $type;
                        ptr = ptr.add(n_channels);
                    }
                }
            }

            &hash_buf[..n_frames * n_channels * mem::size_of::<$type>()]
        }
    };
}

slice_as!(slice_as_i8, i8);
slice_as!(slice_as_i16, i16);
slice_as!(slice_as_i32, i32);