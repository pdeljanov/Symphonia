extern crate core;

mod range;
mod decoder;
mod header;
mod silk;
mod celt;
mod toc;
mod packet;

use symphonia_core::codecs::*;
use symphonia_core::errors::Result;
use symphonia_core::io::*;
use symphonia_core::audio::*;

use decoder::OpusDecoder;

use once_cell::sync::Lazy;

