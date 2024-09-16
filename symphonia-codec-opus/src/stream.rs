use symphonia_core::errors::{Error, Result};
use symphonia_core::formats::Packet;

#[derive(Debug, Clone, Copy)]
pub enum FrameType {
    Silk,
    Hybrid,
    Celt,
}

