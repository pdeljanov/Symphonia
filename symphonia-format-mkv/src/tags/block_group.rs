use std::convert::{TryFrom, TryInto};

use symphonia_core::errors::{Error, Result};
use webm_iterable::matroska_spec::{MatroskaSpec, Block};

#[derive(Debug)]
pub(crate) struct BlockGroup<'a> {
    pub(crate) block: Block<'a>,
    pub(crate) duration: Option<u64>,
}

impl<'a> TryFrom<&'a Vec<MatroskaSpec>> for BlockGroup<'a> {
    type Error = Error;

    fn try_from(tags: &'a Vec<MatroskaSpec>) -> Result<Self> {
        let mut data: Option<Block<'_>> = None;
        let mut block_duration: Option<u64> = None;
        for tag in tags {
            match tag {
                MatroskaSpec::Block(val) => {
                    data = Some(val.as_slice().try_into().or(Err(Error::DecodeError("mkv: unable to decode block")))?);
                },
                MatroskaSpec::BlockDuration(val) => {
                    block_duration = Some(*val);
                },
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }
        Ok(Self {
            block: data.ok_or(Error::DecodeError("mkv: missing block inside block group"))?,
            duration: block_duration,
        })
    }
}