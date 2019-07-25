use super::bitstream::{BitResevoir, next_frame};
use sonata_core::errors::Result;
use sonata_core::io::*;

pub struct Mp3Decoder<B: Bytestream> {
    reader: B,
    resevoir: BitResevoir,
}

impl<B: Bytestream> Mp3Decoder<B> {

    pub fn new(reader: B) -> Self {
        Mp3Decoder {
            reader,
            resevoir: BitResevoir::new(),
        }
    }

    pub fn read_frame(&mut self) -> Result<()> {
        next_frame(&mut self.reader, &mut self.resevoir)
    }

}
