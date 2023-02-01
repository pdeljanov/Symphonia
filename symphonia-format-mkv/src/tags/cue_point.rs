use std::convert::TryFrom;

use symphonia_core::errors::{Error, Result};
use webm_iterable::matroska_spec::{MatroskaSpec, Master};

use super::cue_track_positions::CueTrackPositions;

#[derive(Debug)]
pub(crate) struct CuePoint {
    pub(crate) time: u64,
    pub(crate) positions: CueTrackPositions,
}

impl TryFrom<Vec<MatroskaSpec>> for CuePoint {
    type Error = Error;

    fn try_from(tags: Vec<MatroskaSpec>) -> Result<Self> {
        let mut time = None;
        let mut pos = None;
        for tag in tags {
            match tag {
                MatroskaSpec::CueTime(val) => time = Some(val),
                MatroskaSpec::CueTrackPositions(val) => {
                    if let Master::Full(data) = val {
                        pos = Some(CueTrackPositions::try_from(data)?);
                    }
                }
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            time: time.ok_or(Error::DecodeError("mkv: missing time in cue"))?,
            positions: pos.ok_or(Error::DecodeError("mkv: missing positions in cue"))?,
        })
    }
}