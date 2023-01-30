#![no_main]

use libfuzzer_sys::fuzz_target;

use rustfft::num_complex::Complex;
use symphonia_core::dsp::fft::Fft;

fuzz_target!(|data: Vec<f32>| {
    let input: Vec<Complex<f32>> = data.chunks_exact(2).map(|pair| Complex { re: pair[0], im: pair[1] }).collect();
    let mut fft = Fft::new(input.len());
    fft.fft_inplace(&mut input.clone());
    fft.ifft_inplace(&mut input.clone());
});
