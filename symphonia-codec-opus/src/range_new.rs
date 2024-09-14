use std::cmp;

pub struct Decoder {
    data: Vec<u8>,
    bits_read: u32,
    range_size: u32,              // rng in RFC 6716
    high_and_coded_difference: u32, // val in RFC 6716
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            bits_read: 0,
            range_size: 0,
            high_and_coded_difference: 0,
        }
    }

    pub fn init(&mut self, data: Vec<u8>) {
        self.data = data;
        self.bits_read = 0;
        self.range_size = 128;
        self.high_and_coded_difference = 127 - self.get_bits(7);
        self.normalize();
    }

    pub fn decode_symbol_with_icdf(&mut self, cumulative_distribution_table: &[u32]) -> u32 {
        let total = cumulative_distribution_table[0];
        let cdt = &cumulative_distribution_table[1..];

        let scale = self.range_size / total;
        let mut symbol = self.high_and_coded_difference / scale + 1;
        symbol = total - cmp::min(symbol, total);

        let k = cdt.iter().position(|&x| x > symbol).unwrap_or(cdt.len());

        let high = cdt[k];
        let low = if k != 0 { cdt[k - 1] } else { 0 };

        self.update(scale, low, high, total);
        return k as u32;
    }

    pub fn decode_symbol_log_p(&mut self, logp: u32) -> u32 {
        let scale = self.range_size >> logp;

        return if self.high_and_coded_difference >= scale {
            self.high_and_coded_difference -= scale;
            self.range_size -= scale;
            self.normalize();
            0
        } else {
            self.range_size = scale;
            self.normalize();
            1
        };
    }

    fn get_bit(&mut self) -> u32 {
        let index = (self.bits_read / 8) as usize;
        let offset = self.bits_read % 8;

        if index > self.data.len() - 1 {
            return 0;
        }

        self.bits_read += 1;
        return ((self.data[index] >> (7 - offset)) & 1) as u32;
    }

    fn get_bits(&mut self, n: i32) -> u32 {
        let mut bits = 0;

        for i in 0..n {
            if i != 0 {
                bits <<= 1;
            }
            bits |= self.get_bit();
        }

        return bits;
    }

    fn normalize(&mut self) {
        const MIN_RANGE_SIZE: u32 = 1 << 23;

        while self.range_size <= MIN_RANGE_SIZE {
            self.range_size <<= 8;
            self.high_and_coded_difference = ((self.high_and_coded_difference << 8) + (255 - self.get_bits(8))) & 0x7FFFFFFF;
        }
    }

    fn update(&mut self, scale: u32, low: u32, high: u32, total: u32) {
        self.high_and_coded_difference -= scale * (total - high);
        if low != 0 {
            self.range_size = scale * (high - low);
        } else {
            self.range_size -= scale * (total - high);
        }
        
        return self.normalize();
    }

    pub fn set_internal_values(&mut self, data: Vec<u8>, bits_read: u32, range_size: u32, high_and_coded_difference: u32) {
        self.data = data;
        self.bits_read = bits_read;
        self.range_size = range_size;
        
        return self.high_and_coded_difference = high_and_coded_difference;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SILK_MODEL_FRAME_TYPE_INACTIVE: [u32; 3] = [256, 26, 256];

    const SILK_MODEL_GAIN_HIGHBITS: [[u32; 9]; 3] = [
        [256, 32, 144, 212, 241, 253, 254, 255, 256],
        [256, 2, 19, 64, 124, 186, 233, 252, 256],
        [256, 1, 4, 30, 101, 195, 245, 254, 256],
    ];

    const SILK_MODEL_GAIN_LOWBITS: [u32; 9] = [256, 32, 64, 96, 128, 160, 192, 224, 256];

    const SILK_MODEL_GAIN_DELTA: [u32; 42] = [
        256, 6, 11, 22, 53, 185, 206, 214, 218, 221, 223, 225, 227, 228,
        229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242,
        243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256,
    ];

    const SILK_MODEL_LSF_S1: [[[u32; 33]; 2]; 2] = [
        [
            [
                256, 44, 78, 108, 127, 148, 160, 171, 174, 177, 179,
                195, 197, 199, 200, 205, 207, 208, 211, 214, 215, 216,
                218, 220, 222, 225, 226, 235, 244, 246, 253, 255, 256,
            ],
            [
                256, 1, 11, 12, 20, 23, 31, 39, 53, 66, 80,
                81, 95, 107, 120, 131, 142, 154, 165, 175, 185, 196,
                204, 213, 221, 228, 236, 237, 238, 244, 245, 251, 256,
            ],
        ],
        [
            [
                256, 31, 52, 55, 72, 73, 81, 98, 102, 103, 121,
                137, 141, 143, 146, 147, 157, 158, 161, 177, 188, 204,
                206, 208, 211, 213, 224, 225, 229, 238, 246, 253, 256,
            ],
            [
                256, 1, 5, 21, 26, 44, 55, 60, 74, 89, 90,
                93, 105, 118, 132, 146, 152, 166, 178, 180, 186, 187,
                199, 211, 222, 232, 235, 245, 250, 251, 252, 253, 256,
            ],
        ],
    ];

    const SILK_MODEL_LSF_S2: [[u32; 10]; 16] = [
        [256, 1, 2, 3, 18, 242, 253, 254, 255, 256],
        [256, 1, 2, 4, 38, 221, 253, 254, 255, 256],
        [256, 1, 2, 6, 48, 197, 252, 254, 255, 256],
        [256, 1, 2, 10, 62, 185, 246, 254, 255, 256],
        [256, 1, 4, 20, 73, 174, 248, 254, 255, 256],
        [256, 1, 4, 21, 76, 166, 239, 254, 255, 256],
        [256, 1, 8, 32, 85, 159, 226, 252, 255, 256],
        [256, 1, 2, 20, 83, 161, 219, 249, 255, 256],
        [256, 1, 2, 3, 12, 244, 253, 254, 255, 256],
        [256, 1, 2, 4, 32, 218, 253, 254, 255, 256],
        [256, 1, 2, 5, 47, 199, 252, 254, 255, 256],
        [256, 1, 2, 12, 61, 187, 252, 254, 255, 256],
        [256, 1, 5, 24, 72, 172, 249, 254, 255, 256],
        [256, 1, 2, 16, 70, 170, 242, 254, 255, 256],
        [256, 1, 2, 17, 78, 165, 226, 251, 255, 256],
        [256, 1, 8, 29, 79, 156, 237, 254, 255, 256],
    ];

    const SILK_MODEL_LSF_INTERPOLATION_OFFSET: [u32; 6] = [256, 13, 35, 64, 75, 256];

    const SILK_MODEL_LCG_SEED: [u32; 5] = [256, 64, 128, 192, 256];

    const SILK_MODEL_EXC_RATE: [[u32; 10]; 2] = [
        [256, 15, 66, 78, 124, 169, 182, 215, 242, 256],
        [256, 33, 63, 99, 116, 150, 199, 217, 238, 256],
    ];

    const SILK_MODEL_PULSE_COUNT: [[u32; 19]; 11] = [
        [
            256, 131, 205, 230, 238, 241, 244, 245, 246, 247, 248, 249, 250, 251, 252,
            253, 254, 255, 256,
        ],
        [
            256, 58, 151, 211, 234, 241, 244, 245, 246, 247, 248, 249, 250, 251, 252,
            253, 254, 255, 256,
        ],
        [
            256, 43, 94, 140, 173, 197, 213, 224, 232, 238, 241, 244, 247, 249, 250,
            251, 253, 254, 256,
        ],
        [
            256, 17, 69, 140, 197, 228, 240, 245, 246, 247, 248, 249, 250, 251, 252,
            253, 254, 255, 256,
        ],
        [
            256, 6, 27, 68, 121, 170, 205, 226, 237, 243, 246, 248, 250, 251, 252, 253,
            254, 255, 256,
        ],
        [
            256, 7, 21, 43, 71, 100, 128, 153, 173, 190, 203, 214, 223, 230, 235, 239,
            243, 246, 256,
        ],
        [
            256, 2, 7, 21, 50, 92, 138, 179, 210, 229, 240, 246, 249, 251, 252, 253,
            254, 255, 256,
        ],
        [
            256, 1, 3, 7, 17, 36, 65, 100, 137, 171, 199, 219, 233, 241, 246, 250, 252,
            254, 256,
        ],
        [
            256, 1, 3, 5, 10, 19, 33, 53, 77, 104, 132, 158, 181, 201, 216, 227, 235,
            241, 256,
        ],
        [
            256, 1, 2, 3, 9, 36, 94, 150, 189, 214, 228, 238, 244, 247, 250, 252, 253,
            254, 256,
        ],
        [
            256, 2, 3, 9, 36, 94, 150, 189, 214, 228, 238, 244, 247, 250, 252, 253,
            254, 256, 256,
        ],
    ];

    #[test]
    fn test_decoder() {
        let mut d = Decoder::new();
        d.init(vec![0x0b, 0xe4, 0xc1, 0x36, 0xec, 0xc5, 0x80]);

        assert_eq!(d.decode_symbol_log_p(0x1), 0);
        assert_eq!(d.decode_symbol_log_p(0x1), 0);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_FRAME_TYPE_INACTIVE), 1);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_GAIN_HIGHBITS[0]), 0);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_GAIN_LOWBITS), 6);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_GAIN_DELTA), 0);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_GAIN_DELTA), 3);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_GAIN_DELTA), 4);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_LSF_S1[1][0]), 9);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_LSF_S2[10]), 5);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_LSF_S2[9]), 4);

        for _ in 0..14 {
            assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_LSF_S2[8]), 4);
        }

        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_LSF_INTERPOLATION_OFFSET), 4);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_LCG_SEED), 2);
        assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_EXC_RATE[0]), 0);

        for _ in 0..20 {
            assert_eq!(d.decode_symbol_with_icdf(&SILK_MODEL_PULSE_COUNT[0]), 0);
        }
    }
}