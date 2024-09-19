mod range;
mod decoder;
mod header;
mod silk;
mod celt;
mod packet;

use symphonia_core::codecs::*;
use symphonia_core::errors::Result;
use symphonia_core::io::*;
use symphonia_core::audio::*;

use decoder::OpusDecoder;

use once_cell::sync::Lazy;

