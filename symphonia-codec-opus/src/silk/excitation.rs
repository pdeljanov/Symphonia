use std::ops::Neg;

pub struct Decoder {
    seed: u32,
}

impl Decoder {
    /// # Excitation
    /// 
    /// SILK codes the excitation using a modified version of the Pyramid
    /// Vector Quantizer (PVQ) codebook [PVQ].  The PVQ codebook is designed
    /// for Laplace-distributed values and consists of all sums of K signed,
    /// unit pulses in a vector of dimension N, where two pulses at the same
    /// position are required to have the same sign.  Thus, the codebook
    /// includes all integer codevectors y of dimension N that satisfy
    /// ```text
    ///                               N-1
    ///                               __
    ///                               \  abs(y[j]) = K
    ///                               /_
    ///                               j=0
    ///``` 
    /// Unlike regular PVQ, SILK uses a variable-length, rather than 
    /// fixed-length, encoding.  This encoding is better suited to the more
    /// Gaussian-like distribution of the coefficient magnitudes 
    /// and the non-uniform distribution of their signs 
    /// (caused by the quantization  offset described below).
    /// SILK also handles large codebooks by coding
    /// the least significant bits (LSBs) of each coefficient directly.  
    /// This adds a small coding efficiency loss, but greatly reduces the
    /// computation time and ROM size required for decoding, as implemented
    /// in silk_decode_pulses() (decode_pulses.c).
    /// SILK fixes the dimension of the codebook to N = 16.  The excitation
    /// is made up of a number of "shell blocks", each 16 samples in size.
    /// Table 44 lists the number of shell blocks required for a SILK frame
    /// for each possible audio bandwidth and frame size. 10 ms MB frames
    /// nominally contain 120 samples (10 ms at 12 kHz), which is not a
    /// multiple of 16.  This is handled by coding 8 shell blocks 
    /// (128 samples) and discarding the final 8 samples of the last block.  
    /// The decoder contains no special case that prevents an encoder from
    /// placing pulses in these samples, and they must be correctly parsed
    /// from the bitstream if present, but they are otherwise ignored.
    ///``` 
    ///          +-----------------+------------+------------------------+
    ///          | Audio Bandwidth | Frame Size | Number of Shell Blocks |
    ///          +-----------------+------------+------------------------+
    ///          | NB              | 10 ms      |                      5 |
    ///          |                 |            |                        |
    ///          | MB              | 10 ms      |                      8 |
    ///          |                 |            |                        |
    ///          | WB              | 10 ms      |                     10 |
    ///          |                 |            |                        |
    ///          | NB              | 20 ms      |                     10 |
    ///          |                 |            |                        |
    ///          | MB              | 20 ms      |                     15 |
    ///          |                 |            |                        |
    ///          | WB              | 20 ms      |                     20 |
    ///          +-----------------+------------+------------------------+
    /// 
    ///               Table 44: Number of Shell Blocks Per SILK Frame
    ///```
    pub fn new(initial_seed: u32) -> Self {
        unimplemented!()
    }

    pub fn decode_excitation(
        &mut self,
        e_raw: &[i32],
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
    ) -> Vec<i32> {
        let offset_q23 = Self::get_quantization_offset(signal_type, quantization_offset_type);
        let mut e_q23 = Vec::with_capacity(e_raw.len());

        for &raw_value in e_raw {
            let mut value = (raw_value << 8) - raw_value.signum() * 20 + offset_q23;
            self.seed = self.seed.wrapping_mul(196314165).wrapping_add(907633515);

            if (self.seed & 0x80000000) != 0 {
                value = value.neg();
            }

            self.seed = self.seed.wrapping_add(raw_value as u32);
            e_q23.push(value);
        }

        return e_q23;
    }

    fn get_quantization_offset(
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
    ) -> i32 {
        return match (signal_type, quantization_offset_type) {
            (SignalType::Inactive, QuantizationOffsetType::Low) => 25,
            (SignalType::Inactive, QuantizationOffsetType::High) => 60,
            (SignalType::Unvoiced, QuantizationOffsetType::Low) => 25,
            (SignalType::Unvoiced, QuantizationOffsetType::High) => 60,
            (SignalType::Voiced, QuantizationOffsetType::Low) => 8,
            (SignalType::Voiced, QuantizationOffsetType::High) => 25,
        };
    }
}

#[derive(Clone, Copy)]
pub enum SignalType {
    Inactive,
    Unvoiced,
    Voiced,
}

#[derive(Clone, Copy)]
pub enum QuantizationOffsetType {
    Low,
    High,
}