use symphonia_core::errors::{Error, Result};
use symphonia_core::io::{BitReaderRtl, ReadBitsRtl};

const MIN_RANGE_SIZE: u32 = 1 << 23;

pub struct Decoder<'a> {
    bit_reader: BitReaderRtl<'a>,
    rng: u32,
    val: u32,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Decoder {
            bit_reader: BitReaderRtl::new(buf),
            rng: 0,
            val: 0,
        }
    }

    pub fn init(&mut self) -> Result<()> {
        self.rng = 128;
        let b0 = self.bit_reader.read_bits_leq32(8)?;
        self.val = 127 - (b0 >> 1);
        self.normalize()?;
        Ok(())
    }

    pub fn decode_symbol_with_icdf(&mut self, icdf: &[u32]) -> Result<u32> {
        let ft = icdf[0];
        let scale = self.rng / ft;
        let mut symbol = self.val / scale;
        if symbol * scale == self.val && symbol > 0 {
            symbol -= 1;
        }
        symbol = ft - symbol - 1;

        let mut k = 0;
        while k + 1 < icdf.len() && icdf[k + 1] > symbol {
            k += 1;
        }

        let fl = if k > 0 { icdf[k] } else { 0 };
        let fh = if k + 1 < icdf.len() { icdf[k + 1] } else { ft };

        self.update(scale, fl, fh, ft)?;

        Ok(k as u32)
    }

    pub fn decode_symbol_log_p(&mut self, logp: u32) -> Result<u32> {
        let scale = self.rng >> logp;
        let bit = if self.val >= scale {
            self.val = self.val.wrapping_sub(scale);
            self.rng = self.rng.wrapping_sub(scale);
            0
        } else {
            self.rng = scale;
            1
        };
        self.normalize()?;

        Ok(bit)
    }

    fn normalize(&mut self) -> Result<()> {
        while self.rng <= MIN_RANGE_SIZE {
            self.rng <<= 8;
            let sym = self.bit_reader.read_bits_leq32(8)?;
            self.val = ((self.val << 8) + (255 - sym)) & 0x7FFFFFFF;
        }
        Ok(())
    }

    fn update(&mut self, scale: u32, low: u32, high: u32, total: u32) -> Result<()> {
        self.val = self.val.wrapping_sub(scale.wrapping_mul(total.wrapping_sub(high)));
        if low > 0 {
            self.rng = scale.wrapping_mul(high.wrapping_sub(low));
        } else {
            self.rng = self.rng.wrapping_sub(scale.wrapping_mul(total.wrapping_sub(high)));
        }
        self.normalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_decoder() -> Decoder<'static> {
        let data = &[0x0b, 0xe4, 0xc1, 0x36, 0xec, 0xc5, 0x80];
        let mut decoder = Decoder::new(data);
        decoder.init().unwrap();
        decoder
    }

    #[test]
    fn decode_symbol_with_icdf_should_return_expected_value_for_silk_model_frame_type_inactive() -> Result<()> {
        let mut decoder = setup_decoder();
        let silk_model_frame_type_inactive = &[256, 26, 256];
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_frame_type_inactive)?, 1);
        Ok(())
    }

    #[test]
    fn decode_symbol_with_icdf_should_return_expected_value_for_silk_model_gain_highbits() -> Result<()> {
        let mut decoder = setup_decoder();
        let silk_model_gain_highbits = &[256, 32, 144, 212, 241, 253, 254, 255, 256];
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_gain_highbits)?, 0);
        Ok(())
    }

    #[test]
    fn decode_symbol_with_icdf_should_return_expected_value_for_silk_model_gain_lowbits() -> Result<()> {
        let mut decoder = setup_decoder();
        let silk_model_gain_lowbits = &[256, 32, 64, 96, 128, 160, 192, 224, 256];
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_gain_lowbits)?, 6);
        Ok(())
    }

    #[test]
    fn decode_symbol_with_icdf_should_return_expected_values_for_silk_model_gain_delta() -> Result<()> {
        let mut decoder = setup_decoder();
        let silk_model_gain_delta = &[
            256, 6, 11, 22, 53, 185, 206, 214, 218, 221, 223, 225, 227, 228, 229, 230, 231, 232,
            233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247, 248, 249,
            250, 251, 252, 253, 254, 255, 256,
        ];
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_gain_delta)?, 0);
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_gain_delta)?, 3);
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_gain_delta)?, 4);
        Ok(())
    }

    #[test]
    fn decode_symbol_with_icdf_should_return_expected_value_for_silk_model_lsf_s1() -> Result<()> {
        let mut decoder = setup_decoder();
        let silk_model_lsf_s1 = &[
            256, 31, 52, 55, 72, 73, 81, 98, 102, 103, 121, 137, 141, 143, 146, 147, 157,
            158, 161, 177, 188, 204, 206, 208, 211, 213, 224, 225, 229, 238, 246, 253, 256,
        ];
        assert_eq!(decoder.decode_symbol_with_icdf(silk_model_lsf_s1)?, 9);
        Ok(())
    }

    #[test]
    fn decode_symbol_log_p_should_return_expected_values() -> Result<()> {
        let mut decoder = setup_decoder();
        assert_eq!(decoder.decode_symbol_log_p(1)?, 0);
        assert_eq!(decoder.decode_symbol_log_p(1)?, 0);
        Ok(())
    }
}