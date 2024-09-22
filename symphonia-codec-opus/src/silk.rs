//!  # SILK Decoder
/// The decoder's LP layer uses a modified version of the SILK codec
/// (herein simply called "SILK"), which runs a decoded excitation signal
/// through adaptive long-term and short-term prediction synthesis filters.  
/// It runs at NB, MB, and WB sample rates internally.  When used in a SWB 
/// or FB Hybrid frame, the LP layer itself still only runs in WB.
///
///  ## SILK Decoder Modules
///
///```text
///         +---------+    +------------+
///      -->| Range   |--->| Decode     |---------------------------+
///       1 | Decoder | 2  | Parameters |----------+       5        |
///         +---------+    +------------+     4    |                |
///                             3 |                |                |
///                              \/               \/               \/
///                        +------------+   +------------+   +------------+
///                        | Generate   |-->| LTP        |-->| LPC        |
///                        | Excitation |   | Synthesis  |   | Synthesis  |
///                        +------------+   +------------+   +------------+
///                                                ^                |
///                                                |                |
///                            +-------------------+----------------+
///                            |                                      6
///                            |   +------------+   +-------------+
///                            +-->| Stereo     |-->| Sample Rate |-->
///                                | Unmixing   | 7 | Conversion  | 8
///                                +------------+   +-------------+
///
///      1: Range encoded bitstream
///      2: Coded parameters
///      3: Pulses, LSBs, and signs
///      4: Pitch lags, Long-Term Prediction (LTP) coefficients
///      5: Linear Predictive Coding (LPC) coefficients and gains
///      6: Decoded signal (mono or mid-side stereo)
///      7: Unmixed signal (mono or left-right stereo)
///      8: Resampled signal
///
///
///                           Figure 14: SILK Decoder
/// ```
/// 
/// https://datatracker.ietf.org/doc/html/rfc6716#section-4.2
pub struct Decoder;


impl Decoder {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        unimplemented!();
    }
}