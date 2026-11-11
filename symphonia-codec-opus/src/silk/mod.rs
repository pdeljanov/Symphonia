//! SILK linear-prediction layer
//! ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).
//!
//! The decoder modules and the data passed between them
//! ([RFC 6716 Section 4.2.1, Figure 14](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.1>)):
//!
//! ```text
//!         +---------+    +------------+
//!      -->| Range   |--->| Decode     |---------------------------+
//!       1 | Decoder | 2  | Parameters |----------+       5        |
//!         +---------+    +------------+     4    |                |
//!                             3 |                |                |
//!                              \/               \/               \/
//!                        +------------+   +------------+   +------------+
//!                        | Generate   |-->| LTP        |-->| LPC        |
//!                        | Excitation |   | Synthesis  |   | Synthesis  |
//!                        +------------+   +------------+   +------------+
//!                                                ^                |
//!                                                |                |
//!                            +-------------------+----------------+
//!                            |                                      6
//!                            |   +------------+   +-------------+
//!                            +-->| Stereo     |-->| Sample Rate |-->
//!                                | Unmixing   | 7 | Conversion  | 8
//!                                +------------+   +-------------+
//!
//!   1: range-coded bitstream             5: LPC coefficients and gains
//!   2: coded parameters                  6: decoded signal (mono or mid-side stereo)
//!   3: pulses, LSBs and signs            7: unmixed signal (mono or left-right stereo)
//!   4: pitch lags, LTP coefficients      8: resampled signal
//! ```
//!
//! Submodules, each with the RFC 6716 sections it implements:
//! - [`constant`]: Codebooks, inverse-CDF tables and filter coefficients
//!   ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
//! - [`params`]: Header flags, stereo prediction weights, mid-only flag, frame type, subframe
//!   gains and LTP parameters
//!   ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
//!   [Section 4.2.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>) to
//!   [Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
//!   [Section 4.2.7.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>)).
//! - [`lsf`]: Two-stage normalized line spectral frequency decoding, stabilization and
//!   interpolation
//!   ([RFC 6716 Section 4.2.7.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>)).
//! - [`excitation`]: LCG seed, rate level, pulse counts, shell-coded pulse locations, LSBs and
//!   signs
//!   ([RFC 6716 Section 4.2.7.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.7>),
//!   [Section 4.2.7.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>)).
//! - [`lpc`]: NLSF to LPC conversion, coefficient limiting and the prediction kernels shared by
//!   frame reconstruction and concealment
//!   ([RFC 6716 Section 4.2.7.5.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>)
//!   to [Section 4.2.7.5.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.8>),
//!   [Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>)).
//! - [`synthesis`]: LTP and LPC synthesis of a frame and mid-side stereo unmixing
//!   ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//!   [Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
//! - [`resampler`]: Conversion from the internal SILK rate to the output rate
//!   ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
//! - [`plc`]: Packet loss concealment
//!   ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//! - [`cng`]: Comfort noise generation for DTX gaps and lost frames
//!   ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//! - [`frame`]: The data of one SILK frame: geometry, header flags, stereo weights, frame and
//!   subframe parameters
//!   ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>) to
//!   [Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
//! - [`state`]: The state carried across frames: per-channel decoder state, stereo history and
//!   coding references
//!   ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//!   [Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
//! - [`decoder`]: Per-channel state and the LBRR and regular frame schedule of an Opus frame
//!   ([RFC 6716 Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>) to
//!   [Section 4.2.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.6>)).
//! - [`error`]: Errors of the SILK layer.

pub(crate) mod cng;
pub(crate) mod constant;
pub(crate) mod decoder;
pub(crate) mod error;
pub(crate) mod excitation;
pub(crate) mod frame;
pub(crate) mod lpc;
pub(crate) mod lsf;
pub(crate) mod params;
pub(crate) mod plc;
pub(crate) mod resampler;
pub(crate) mod state;
pub(crate) mod synthesis;

pub(crate) use decoder::Decoder;
pub(crate) use frame::*;
pub(crate) use resampler::ResamplerState;
pub(crate) use state::*;

/// Maximum number of 20-ms SILK frames inside a single Opus frame (60 ms = 3 x 20 ms).
pub(crate) const MAX_SILK_FRAMES: usize = 3;

/// Maximum number of 5-ms subframes inside a single SILK frame (20 ms = 4 x 5 ms).
pub(crate) const MAX_SUBFRAMES: usize = 4;

/// Maximum LPC filter order (16 for WB, 10 for NB/MB).
pub(crate) const MAX_LPC_ORDER: usize = 16;

/// Number of taps in the voiced Long-Term Prediction (LTP) filter.
pub(crate) const LTP_ORDER: usize = 5;

/// Maximum number of samples in a single 5-ms SILK subframe (80 samples at 16 kHz WB).
pub(crate) const MAX_SUBFRAME_SAMPLES: usize = 80;

/// Maximum number of samples in a single 20-ms SILK frame (320 samples at 16 kHz WB).
pub(crate) const MAX_FRAME_SAMPLES: usize = MAX_SUBFRAMES * MAX_SUBFRAME_SAMPLES;

/// Number of samples in each shell-coding excitation block.
pub(crate) const SHELL_BLOCK_SAMPLES: usize = 16;

/// Maximum number of 16-sample shell blocks in a single SILK frame (`320 / 16 = 20`).
pub(crate) const MAX_SHELL_BLOCKS: usize = MAX_FRAME_SAMPLES / SHELL_BLOCK_SAMPLES;

/// LTP memory duration in milliseconds (`LTP_MEM_LENGTH_MS = 20`).
pub(crate) const LTP_MEM_LENGTH_MS: usize = 20;

/// Maximum number of samples in the LTP history buffer (`20 ms * 16 kHz = 320`).
pub(crate) const MAX_LTP_MEM_SAMPLES: usize = 320;

/// Length of per-channel output history buffer (`MAX_FRAME_SAMPLES + 2 * MAX_SUBFRAME_SAMPLES = 480`).
pub(crate) const OUT_BUF_SAMPLES: usize = MAX_FRAME_SAMPLES + 2 * MAX_SUBFRAME_SAMPLES;
