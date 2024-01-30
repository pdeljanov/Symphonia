use std::convert::TryFrom;

use symphonia_core::errors::{Error, Result};
use webm_iterable::matroska_spec::MatroskaSpec;

#[derive(Debug)]
pub(crate) struct Audio {
    pub(crate) sampling_frequency: f64,
    pub(crate) channels: u64,
    pub(crate) bit_depth: Option<u64>,
}

impl TryFrom<Vec<MatroskaSpec>> for Audio {
    type Error = Error;
    fn try_from(tags: Vec<MatroskaSpec>) -> Result<Self> {
        let mut sampling_frequency = None;
        let mut channels = None;
        let mut bit_depth = None;

        for tag in tags {
            match tag {
                MatroskaSpec::SamplingFrequency(val) => {
                    sampling_frequency = Some(val);
                },
                MatroskaSpec::Channels(val) => {
                    channels = Some(val);
                },
                MatroskaSpec::BitDepth(val) => {
                    bit_depth = Some(val);
                },
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            sampling_frequency: sampling_frequency.unwrap_or(8000.0),
            channels: channels.unwrap_or(1),
            bit_depth,
        })
    }    
}