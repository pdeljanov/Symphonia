use std::convert::TryFrom;

use symphonia_core::errors::{Error, Result};
use webm_iterable::matroska_spec::MatroskaSpec;

#[derive(Debug)]
pub(crate) struct Info {
    pub(crate) timestamp_scale: u64,
    pub(crate) duration: Option<f64>,
}

impl TryFrom<Vec<MatroskaSpec>> for Info {
    type Error = Error;

    fn try_from(tags: Vec<MatroskaSpec>) -> Result<Self> {
        let mut duration = None;
        let mut timestamp_scale = None;

        for tag in tags {
            match tag {
                MatroskaSpec::TimestampScale(val) => {
                    timestamp_scale = Some(val);
                },
                MatroskaSpec::Duration(val) => {
                    duration = Some(val);
                },
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            timestamp_scale: timestamp_scale.unwrap_or(1_000_000),
            duration
        })
    }
}