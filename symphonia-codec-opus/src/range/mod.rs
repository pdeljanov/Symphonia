use once_cell::sync::Lazy;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

const LOOKUP_TABLE_SIZE: usize = 256;
static NORMALIZE_TABLES: Lazy<NormalizeTables> = Lazy::new(NormalizeTables::new);


struct NormalizeTables {
    shift: [u8; LOOKUP_TABLE_SIZE],
    add: [u32; LOOKUP_TABLE_SIZE],
}

impl NormalizeTables {
    fn new() -> Self {
        let mut shift = [0u8; LOOKUP_TABLE_SIZE];
        let mut add = [0u32; LOOKUP_TABLE_SIZE];

        for i in 0..LOOKUP_TABLE_SIZE {
            let mut s = 0u8;
            let mut a = 0u32;
            let mut v = i;

            while v < 128 {
                v <<= 1;
                s = s.saturating_add(1);
                if a == 0x7FFFFFFF {
                    break;
                }
                a = (a << 1) | 1;
            }

            shift[i] = s;
            add[i] = a;
        }

        return NormalizeTables { shift, add };
    }
}

/// Decoder implements rfc6716#section-4.1
/// Opus uses an entropy coder based on range coding [RANGE-CODING]
/// [MARTIN79], which is itself a rediscovery of the FIFO arithmetic code
/// introduced by [CODING-THESIS].  It is very similar to arithmetic
/// encoding, except that encoding is done with digits in any base
/// instead of with bits, so it is faster when using larger bases (i.e.,
/// a byte).  All of the calculations in the range coder must use bit-
/// exact integer arithmetic.
pub struct Decoder<'a, B: ReadBytes> {
    buf: &'a mut B,
    range: u32,
    value: u32,
    bits_read: u32,
    current_byte: u8,
}

impl<'a, B: ReadBytes> Decoder<'a, B> {
    /// Creates a new Opus Range Decoder and initializes its state.
    ///
    /// Let b0 be an 8-bit unsigned integer containing first input byte (or
    /// containing zero if there are no bytes in this Opus frame).
    /// The decoder initializes rng to 128 and initializes val to
    /// (127 - (b0>>1)), where (b0>>1) is the top 7 bits of the first input byte.
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.1
    pub fn new(buf: &'a mut B) -> Result<Self> {
        let mut decoder = Decoder {
            buf,
            range: 128 << 23,
            value: 0,
            bits_read: 0,
            current_byte: 0,
        };

        decoder.value = match decoder.get_bits(8) {
            Ok(bits) => 127 - (bits >> 1),
            Err(err) => return Err(err),
        };

        decoder.normalize()?;

        return Ok(decoder);
    }

    /// DecodeSymbolWithICDF decodes a single symbol with a table-based context of up to 8 bits.
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.3
    pub fn decode_symbol_with_icdf(&mut self, cdf: &[u32]) -> Result<u32> {
        if cdf.len() < 2 {
            return Err(Error::DecodeError("Invalid CDF"));
        }

        let ft = cdf[0];
        if ft == 0 || ft > 32768 {
            return Err(Error::DecodeError("Invalid CDF total frequency"));
        }

        let scale = self.range / ft;
        let mut symbol = self.value / scale;

        symbol = ft - u32::min(symbol, ft);

        let (k, fl, fh) = cdf.windows(2)
            .enumerate()
            .find(|(_, window)| window[1] > symbol)
            .map(|(i, window)| (i, window[0], window[1]))
            .unwrap_or((cdf.len() - 1, cdf[cdf.len() - 2], cdf[cdf.len() - 1]));

        self.value -= scale * (ft - fh);
        if fl > 0 {
            self.range = scale * (fh - fl);
        } else {
            self.range -= scale * (ft - fh);
        }

        self.normalize()?;

        return Ok(k as u32);
    }

    /// DecodeSymbolLogP decodes a single binary symbol.
    ///
    /// The context is described by a single parameter, logp, which
    /// is the absolute value of the base-2 logarithm of the probability of a "1".
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.2
    pub fn decode_symbol_log_p(&mut self, logp: u32) -> Result<bool> {
        if logp > 31 {
            return Err(Error::DecodeError("Invalid logp value"));
        }

        let scale = if logp == 0 { self.range } else { self.range >> logp };
        let bit = self.value >= scale;

        if bit {
            self.value -= scale;
            self.range -= scale;
        } else {
            self.range = scale;
        }

        self.normalize()?;

        return Ok(bit);
    }

    /// Normalizes the range as described in RFC 6716, Section 4.1.2.1.
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2.1
    /// To normalize the range, the decoder repeats the following process,
    /// until rng > 2**23. If rng is already greater than 2**23, 
    /// the entire process is skipped.
    /// for the initialization used to process the first byte. 
    /// Then, it sets val = ((val<<8) + (255-sym)) & 0x7FFFFFFF
    fn normalize(&mut self) -> Result<()> {
        while self.range <= 0x00FFFFFF {
            self.range <<= 8;
            self.value = (self.value << 8) | (self.get_bits(8)? as u32);
        }
        
        return Ok(());
    }

    fn get_bits(&mut self, n: u32) -> Result<u32> {
        match n {
            0 => return Ok(0),
            n if n > 32 => return Err(Error::DecodeError("Invalid number of bits requested")),
            _ => {}
        }

        (0..n).try_fold(0u32, |acc, _| {
            match self.get_bit() {
                Ok(bit) => Ok((acc << 1) | bit),
                Err(Error::IoError(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    Ok(acc << 1)  // Pad with 0 if we've reached the end of the buffer
                }
                Err(err) => Err(err),
            }
        })
    }

    fn get_bit(&mut self) -> Result<u32> {
        if self.bits_read % 8 == 0 {
            match self.buf.read_byte() {
                Ok(byte) => self.current_byte = byte,
                Err(err) => return Err(Error::IoError(err)),
            }
        }

        let bit = (self.current_byte >> (7 - self.bits_read % 8)) & 1;
        self.bits_read += 1;

        return Ok(bit as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;


    const SILK_MODEL_FRAME_TYPE_INACTIVE: &[u32] = &[256, 26, 256];

    const SILK_MODEL_GAIN_HIGHBITS: &[&[u32]] = &[
        &[256, 32, 144, 212, 241, 253, 254, 255, 256],
        &[256, 2, 19, 64, 124, 186, 233, 252, 256],
        &[256, 1, 4, 30, 101, 195, 245, 254, 256],
    ];

    const SILK_MODEL_GAIN_LOWBITS: &[u32] = &[256, 32, 64, 96, 128, 160, 192, 224, 256];

    const SILK_MODEL_GAIN_DELTA: &[u32] = &[
        256, 6, 11, 22, 53, 185, 206, 214, 218, 221, 223, 225, 227, 228,
        229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242,
        243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256,
    ];

    const SILK_MODEL_LSF_S1: &[&[&[u32]]] = &[
        &[
            &[
                256, 44, 78, 108, 127, 148, 160, 171, 174, 177, 179,
                195, 197, 199, 200, 205, 207, 208, 211, 214, 215, 216,
                218, 220, 222, 225, 226, 235, 244, 246, 253, 255, 256,
            ],
            &[
                256, 1, 11, 12, 20, 23, 31, 39, 53, 66, 80,
                81, 95, 107, 120, 131, 142, 154, 165, 175, 185, 196,
                204, 213, 221, 228, 236, 237, 238, 244, 245, 251, 256,
            ],
        ],
        &[
            &[
                256, 31, 52, 55, 72, 73, 81, 98, 102, 103, 121,
                137, 141, 143, 146, 147, 157, 158, 161, 177, 188, 204,
                206, 208, 211, 213, 224, 225, 229, 238, 246, 253, 256,
            ],
            &[
                256, 1, 5, 21, 26, 44, 55, 60, 74, 89, 90,
                93, 105, 118, 132, 146, 152, 166, 178, 180, 186, 187,
                199, 211, 222, 232, 235, 245, 250, 251, 252, 253, 256,
            ],
        ],
    ];

    const SILK_MODEL_LSF_S2: &[&[u32]] = &[
        &[256, 1, 2, 3, 18, 242, 253, 254, 255, 256],
        &[256, 1, 2, 4, 38, 221, 253, 254, 255, 256],
        &[256, 1, 2, 6, 48, 197, 252, 254, 255, 256],
        &[256, 1, 2, 10, 62, 185, 246, 254, 255, 256],
        &[256, 1, 4, 20, 73, 174, 248, 254, 255, 256],
        &[256, 1, 4, 21, 76, 166, 239, 254, 255, 256],
        &[256, 1, 8, 32, 85, 159, 226, 252, 255, 256],
        &[256, 1, 2, 20, 83, 161, 219, 249, 255, 256],
        &[256, 1, 2, 3, 12, 244, 253, 254, 255, 256],
        &[256, 1, 2, 4, 32, 218, 253, 254, 255, 256],
        &[256, 1, 2, 5, 47, 199, 252, 254, 255, 256],
        &[256, 1, 2, 12, 61, 187, 252, 254, 255, 256],
        &[256, 1, 5, 24, 72, 172, 249, 254, 255, 256],
        &[256, 1, 2, 16, 70, 170, 242, 254, 255, 256],
        &[256, 1, 2, 17, 78, 165, 226, 251, 255, 256],
        &[256, 1, 8, 29, 79, 156, 237, 254, 255, 256],
    ];

    const SILK_MODEL_LSF_INTERPOLATION_OFFSET: &[u32] = &[256, 13, 35, 64, 75, 256];
    const SILK_MODEL_LCG_SEED: &[u32] = &[256, 64, 128, 192, 256];
    const SILK_MODEL_EXC_RATE: &[&[u32]] = &[
        &[256, 15, 66, 78, 124, 169, 182, 215, 242, 256],
        &[256, 33, 63, 99, 116, 150, 199, 217, 238, 256],
    ];

    const SILK_MODEL_PULSE_COUNT: &[&[u32]] = &[
        &[256, 131, 205, 230, 238, 241, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256],
        &[256, 58, 151, 211, 234, 241, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256],
        &[256, 43, 94, 140, 173, 197, 213, 224, 232, 238, 241, 244, 247, 249, 250, 251, 253, 254, 256],
        &[256, 17, 69, 140, 197, 228, 240, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256],
        &[256, 6, 27, 68, 121, 170, 205, 226, 237, 243, 246, 248, 250, 251, 252, 253, 254, 255, 256],
        &[256, 7, 21, 43, 71, 100, 128, 153, 173, 190, 203, 214, 223, 230, 235, 239, 243, 246, 256],
        &[256, 2, 7, 21, 50, 92, 138, 179, 210, 229, 240, 246, 249, 251, 252, 253, 254, 255, 256],
        &[256, 1, 3, 7, 17, 36, 65, 100, 137, 171, 199, 219, 233, 241, 246, 250, 252, 254, 256],
        &[256, 1, 3, 5, 10, 19, 33, 53, 77, 104, 132, 158, 181, 201, 216, 227, 235, 241, 256],
        &[256, 1, 2, 3, 9, 36, 94, 150, 189, 214, 228, 238, 244, 247, 250, 252, 253, 254, 256],
        &[256, 2, 3, 9, 36, 94, 150, 189, 214, 228, 238, 244, 247, 250, 252, 253, 254, 256, 256],
    ];

    #[test]
    fn decode_symbol_with_icdf_empty_cdf() -> Result<()> {
        let data = [0xFF];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        let result = decoder.decode_symbol_with_icdf(&[]);
        assert!(result.is_err());

        return Ok(());
    }

    #[test]
    fn decode_symbol_log_p_edge_cases() -> Result<()> {
        let data = [0xFF, 0xFF];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        assert!(decoder.decode_symbol_log_p(0)?);
        assert!(decoder.decode_symbol_log_p(31)?);
        assert!(decoder.decode_symbol_log_p(32).is_err());

        return Ok(());
    }


    #[test]
    fn get_bits_zero() -> Result<()> {
        let data = [0xFF];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        assert_eq!(decoder.get_bits(0)?, 0);
        return Ok(());
    }

    struct TestReader<'a> {
        data: &'a [u8],
        position: usize,
    }

    impl<'a> ReadBytes for TestReader<'a> {
        fn read_byte(&mut self) -> io::Result<u8> {
            if self.position < self.data.len() {
                let byte = self.data[self.position];
                self.position += 1;
                return Ok(byte);
            }

            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF"));
        }


        fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
            let mut buf = [0u8; 2];
            buf[0] = self.read_byte()?;
            buf[1] = self.read_byte()?;
            return Ok(buf);
        }

        fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
            let mut buf = [0u8; 3];
            buf[0] = self.read_byte()?;
            buf[1] = self.read_byte()?;
            buf[2] = self.read_byte()?;
            return Ok(buf);
        }

        fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
            let mut buf = [0u8; 4];
            buf[0] = self.read_byte()?;
            buf[1] = self.read_byte()?;
            buf[2] = self.read_byte()?;
            buf[3] = self.read_byte()?;
            return Ok(buf);
        }

        fn read_buf(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            for (i, byte) in buf.iter_mut().enumerate() {
                match self.read_byte() {
                    Ok(b) => *byte = b,
                    Err(err) => return Ok(i),
                }
            }
            return Ok(buf.len());
        }

        fn read_buf_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
            for byte in buf.iter_mut() {
                *byte = self.read_byte()?;
            }
            return Ok(());
        }

        fn scan_bytes_aligned<'b>(&mut self, _pattern: &[u8], _align: usize, _buf: &'b mut [u8]) -> io::Result<&'b mut [u8]> {
            unimplemented!("scan_bytes_aligned is not used in this test");
        }

        fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {
            self.position += count as usize;
            return Ok(());
        }

        fn pos(&self) -> u64 {
            return self.position as u64;
        }
    }

    #[test]
    fn decoder() -> Result<()> {
        let data = [0x0b, 0xe4, 0xc1, 0x36, 0xec, 0xc5, 0x80];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        assert!(!decoder.decode_symbol_log_p(1)?, "DecodeSymbolLogP failed");
        assert!(!decoder.decode_symbol_log_p(1)?, "DecodeSymbolLogP failed");
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_FRAME_TYPE_INACTIVE)?, 1);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_GAIN_HIGHBITS[0])?, 0);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_GAIN_LOWBITS)?, 6);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_GAIN_DELTA)?, 0);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_GAIN_DELTA)?, 3);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_GAIN_DELTA)?, 4);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_LSF_S1[1][0])?, 9);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_LSF_S2[10])?, 5);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_LSF_S2[9])?, 4);

        for _ in 0..12 {
            assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_LSF_S2[8])?, 4);
        }

        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_LSF_INTERPOLATION_OFFSET)?, 4);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_LCG_SEED)?, 2);
        assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_EXC_RATE[0])?, 0);

        for _ in 0..20 {
            assert_eq!(decoder.decode_symbol_with_icdf(SILK_MODEL_PULSE_COUNT[0])?, 0);
        }

        return Ok(());
    }


    #[test]
    fn decoder_error_handling() -> Result<()> {
        let data = [0x0b];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        let result = decoder.decode_symbol_with_icdf(SILK_MODEL_FRAME_TYPE_INACTIVE);
        assert!(result.is_err(), "Expected an error due to insufficient data");

        return Ok(());
    }

    #[test]
    fn decoder_edge_cases() -> Result<()> {
        let data = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        let edge_icdf = &[256, 255, 256];
        assert_eq!(decoder.decode_symbol_with_icdf(edge_icdf)?, 1);

        assert!(decoder.decode_symbol_log_p(31)?);
        assert!(decoder.decode_symbol_log_p(0)?);
        assert!(decoder.decode_symbol_log_p(32).is_err());

        return Ok(());
    }

    #[test]
    fn decoder_consistency() -> Result<()> {
        let data = [0x0b, 0xe4, 0xc1, 0x36, 0xec, 0xc5, 0x80];
        let mut reader1 = TestReader { data: &data, position: 0 };
        let mut reader2 = TestReader { data: &data, position: 0 };
        let mut decoder1 = Decoder::new(&mut reader1)?;
        let mut decoder2 = Decoder::new(&mut reader2)?;

        for _ in 0..10 {
            let result1 = decoder1.decode_symbol_with_icdf(SILK_MODEL_FRAME_TYPE_INACTIVE)?;
            let result2 = decoder2.decode_symbol_with_icdf(SILK_MODEL_FRAME_TYPE_INACTIVE)?;
            assert_eq!(result1, result2, "Inconsistent results between decoders");
        }

        return Ok(());
    }

    #[test]
    fn get_bits() -> Result<()> {
        let data = [0xA5, 0x5A];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        assert_eq!(decoder.get_bits(0)?, 0);
        assert_eq!(decoder.get_bits(1)?, 1);
        assert_eq!(decoder.get_bits(7)?, 0x25);
        assert_eq!(decoder.get_bits(8)?, 0x5A);
        assert!(decoder.get_bits(33).is_err());

        return Ok(());
    }

    #[test]
    fn normalize() -> Result<()> {
        let data = [0xFF, 0xFF, 0xFF, 0xFF];
        let mut reader = TestReader { data: &data, position: 0 };
        let mut decoder = Decoder::new(&mut reader)?;

        decoder.range = 0x00FFFFFF;
        decoder.normalize()?;
        assert!(decoder.range > 0x00FFFFFF);

        return Ok(());
    }
}
        