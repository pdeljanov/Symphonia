use std::convert::TryFrom;

use symphonia_core::errors::{Error, Result};
use webm_iterable::matroska_spec::MatroskaSpec;

#[derive(Debug)]
pub(crate) struct CueTrackPositions {
    pub(crate) cluster_position: u64,
}

impl TryFrom<Vec<MatroskaSpec>> for CueTrackPositions {
    type Error = Error;

    fn try_from(tags: Vec<MatroskaSpec>) -> Result<Self> {
        let mut pos = None;
        for tag in tags {
            match tag {
                MatroskaSpec::CueClusterPosition(val) => {
                    pos = Some(val);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }
        Ok(Self {
            cluster_position: pos
                .ok_or(Error::DecodeError("mkv: missing position in cue track positions"))?,
        })
    }
}