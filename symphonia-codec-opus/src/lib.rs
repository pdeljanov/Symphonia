#![allow(clippy::needless_return)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::upper_case_acronyms)]
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unreachable_patterns)]

mod celt;
mod entropy;
mod decoder;
mod header;
mod silk;
mod toc;
mod packet;

pub use decoder::OpusDecoder;