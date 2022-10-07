/// `Nibble` represents the lower or upper 4 bits of a byte
pub(crate) enum Nibble {
    Upper,
    Lower,
}

impl Nibble {
    pub fn get_nibble(&self, byte: u8) -> u8 {
        match self {
            Nibble::Upper => byte >> 4,
            Nibble::Lower => byte & 0x0F,
        }
    }
}

macro_rules! i16_to_i32 {
    ($input:expr) => {
        $input as i16 as i32
    };
}

macro_rules! from_i16_shift {
    ($input:expr) => {
        ($input as i32) << 16
    };
}

pub(crate) use from_i16_shift;
pub(crate) use i16_to_i32;
