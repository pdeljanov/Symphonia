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
use std::slice;
use std::mem;
use md5;
use sonata_core::audio::{AudioBuffer, Signal};

/// `Md5AudioValidator` computes the MD5 checksum of an audio stream taking into account the peculiarities of FLAC's
/// MD5 validation scheme.
pub struct Md5AudioValidator {
    state: md5::Context,
    format_buf: Vec<u8>,
}

impl Md5AudioValidator {

    pub fn new() -> Self {
        Md5AudioValidator {
            state: md5::Context::new(),
            format_buf: Vec::new(),
        }
    }

    pub fn update(&mut self, buf: &AudioBuffer<i32>, bps: u32) {
        // The MD5 checksum is calculated on the final interleaved audio samples of the correct sample format.
        // Sonata's AudioBuffer's are in planar format, and the FLAC decoder works internally on signed 32-bit samples
        // exclusively. Therefore, the samples must be converted to the final format and interleaved to perform
        // validation.

        let bytes_per_sample = match bps {
            0       => return,
            1..=8   => 1,
            9..=16  => 2,
            17..=24 => 3,
            25..=32 => 4,
            _ => unreachable!(),
        };

        let n_channels = 2;
        let n_frames = buf.frames();
        let n_samples = n_channels * n_frames;

        // Calculate the number of bytes required to store all the converted samples.
        let bytes_required = bytes_per_sample * n_channels * n_frames;

        // eprintln!("Validate: n_samples={}, n_channels={}, bytes_per_sample={}, bytes_required={}",
        //     n_samples,
        //     n_channels,
        //     bytes_per_sample,
        //     bytes_required);

        // Resize the formatting buffer to ensure there is enough capacity for all the converted samples.
        if self.format_buf.len() < bytes_required {
            self.format_buf.resize(bytes_required, 0u8);
        }

        let compute_slice = match bytes_per_sample {
            1 => slice_format_as_i8(&mut self.format_buf, buf, n_samples, n_frames),
            2 => slice_format_as_i16(&mut self.format_buf, buf, n_samples, n_frames),
            3 => slice_format_as_i24(&mut self.format_buf, buf, n_samples, n_frames),
            4 => slice_format_as_i32(&mut self.format_buf, buf, n_samples, n_frames),
            _ => unreachable!(),
        };

        self.state.consume(compute_slice);
    }

    pub fn finalize(&mut self) -> md5::Digest {
        self.state.clone().compute()
    }
}

  fn slice_format_as_i8<'a>(out_buf: &'a mut [u8], buf: &AudioBuffer<i32>, n_samples: usize, n_frames: usize) -> &'a [u8] {
        unsafe {
            let format_buf = slice::from_raw_parts_mut(out_buf.as_mut_ptr() as *mut i8, n_samples);
            let mut buffer_planes = buf.planes();

            let mut k = 0;
            for i in 0..n_frames {
                for plane in buffer_planes.planes() {
                    *format_buf.get_unchecked_mut(k) = plane[i] as i8;
                    k += 1;
                }
            }
        }
        &out_buf[..n_samples * mem::size_of::<i8>()]
    }

    fn slice_format_as_i16<'a>(out_buf: &'a mut [u8], buf: &AudioBuffer<i32>, n_samples: usize, n_frames: usize) -> &'a [u8] {
        unsafe {
            let format_buf = slice::from_raw_parts_mut(out_buf.as_mut_ptr() as *mut i16, n_samples);
            let mut buffer_planes = buf.planes();

            let mut k = 0;
            for i in 0..n_frames {
                for plane in buffer_planes.planes() {
                    *format_buf.get_unchecked_mut(k) = plane[i]  as i16;
                    k += 1;
                }
            }
        }
        &out_buf[..n_samples * mem::size_of::<i16>()]
    }

    fn slice_format_as_i24<'a>(out_buf: &'a mut [u8], buf: &AudioBuffer<i32>, n_samples: usize, n_frames: usize) -> &'a [u8] {
        let mut buffer_planes = buf.planes();

        let mut k = 0;
        for i in 0..n_frames {
            for plane in buffer_planes.planes() {
                unsafe {
                    let sample = plane[i];

                    *out_buf.get_unchecked_mut(k) = (sample & 0x0000ff) as u8;
                    *out_buf.get_unchecked_mut(k + 1) = ((sample & 0x00ff00) >> 8) as u8;
                    *out_buf.get_unchecked_mut(k + 2) = ((sample & 0xff0000) >> 16) as u8;
                    k += 3;
                }
            }
        }

        &out_buf[..n_samples * 3]
    }

    fn slice_format_as_i32<'a>(out_buf: &'a mut [u8], buf: &AudioBuffer<i32>, n_samples: usize, n_frames: usize) -> &'a [u8] {
        unsafe {
            let format_buf = slice::from_raw_parts_mut(out_buf.as_mut_ptr() as *mut i32, n_samples);
            let mut buffer_planes = buf.planes();

            let mut k = 0;
            for i in 0..n_frames {
                for plane in buffer_planes.planes() {
                    *format_buf.get_unchecked_mut(k) = plane[i];
                    k += 1;
                }
            }
        }

        &out_buf[..n_samples * mem::size_of::<i32>()]
    }
