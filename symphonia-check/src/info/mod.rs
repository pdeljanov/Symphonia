// Symphonia Check Tool
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// Justification: Fields on DecoderOptions and FormatOptions may change at any time, but
// symphonia-check doesn't want to be updated every time those fields change, therefore always fill
// in the remaining fields with default values.
#![allow(clippy::needless_update)]

use std::cmp::max;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use log::warn;
use mediainfo::{build_mediainfo_command, get_mediainfo_format};
use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::codecs::audio::{well_known::*, AudioCodecParameters};
use symphonia::core::codecs::subtitle::SubtitleCodecId;
use symphonia::core::codecs::subtitle::{well_known::*, SubtitleCodecParameters};
use symphonia::core::codecs::video::VideoCodecId;
use symphonia::core::codecs::video::{well_known::*, VideoCodecParameters};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::{unsupported_error, Result};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
mod mediainfo;
use crate::{InfoTestDecoder, InfoTestOptions, RefProcess};

const EMPTY: &str = "---";

struct Line {
    title: String,
    act: String,
    exp: String,
}

impl Line {
    fn new(title: &str, act: &str, exp: &str) -> Self {
        Self { title: title.to_string(), act: act.to_string(), exp: exp.to_string() }
    }

    fn new_s(title: String, act: String, exp: String) -> Self {
        Self { title, act, exp }
    }

    fn new_line(title: &str) -> Self {
        Self { title: title.to_string(), act: "".to_string(), exp: "".to_string() }
    }
}

fn get_ref_decoder_format(opts: &InfoTestOptions) -> Result<Box<dyn FormatReader>> {
    match opts.ref_decoder {
        InfoTestDecoder::Mediainfo => get_mediainfo_format(opts),
    }
}

/// returns a symphonia FormatReader object for a file
fn get_symphonia_format(opts: &InfoTestOptions) -> Result<Box<dyn FormatReader>> {
    let tgt_ms = Box::new(File::open(Path::new(&opts.input))?);
    let tgt_mss = MediaSourceStream::new(tgt_ms, Default::default());
    let tgt_fmt_opts = Default::default();
    let meta_opts: MetadataOptions = Default::default();
    let hint = Hint::new();
    let format = symphonia::default::get_probe().probe(&hint, tgt_mss, tgt_fmt_opts, meta_opts)?;
    Ok(format)
}

/// returns text output lines from the reference decoder
fn get_ref_decoder_output(opts: &InfoTestOptions) -> Result<String> {
    // Start the mediainfo process.
    let mut ref_process = match opts.ref_decoder {
        InfoTestDecoder::Mediainfo => RefProcess::try_spawn(build_mediainfo_command(&opts.input))?,
    };

    // Instantiate a reader for the mediainfo process output.
    let mut ref_reader = BufReader::new(ref_process.child.stdout.take().unwrap());

    // Read all output to multiline String
    let mut output = String::new();
    ref_reader.read_to_string(&mut output)?;

    Ok(output)
}

pub fn run_info(opts: InfoTestOptions) -> Result<()> {
    // consider ref decoder as expected value.
    // ref decoder output is processed and converted into symphonia FormatReader for comparison.
    let exp = get_ref_decoder_format(&opts)?;

    // consider symphonia detection as actual
    let act = get_symphonia_format(&opts)?;

    // collect the differencies in lines to display them at the end
    let mut diff_lines = Vec::new();

    if exp.format_info().format != act.format_info().format {
        // "General" section contains overall information about the file
        diff_lines.push(Line::new_line("General"));
        diff_lines.push(Line::new(
            "Format",
            act.format_info().short_name,
            exp.format_info().short_name,
        ));
    }

    let exp_tracks = exp.tracks();
    let mut act_tracks = Vec::new();
    act_tracks.extend(act.tracks());
    // sort tracks, before comparison, some files don't have tracks in usual order
    act_tracks.sort_by_key(|track| match track.codec_params {
        Some(CodecParameters::Video(_)) => 0,    // Video first
        Some(CodecParameters::Audio(_)) => 1,    // Audio second
        Some(CodecParameters::Subtitle(_)) => 2, // Subtitle third
        Some(_) | None => 4,                     // None last
    });
    let max = max(exp_tracks.len(), act_tracks.len());
    for i in 0..max {
        compare_tracks(
            &mut diff_lines,
            i + 1, // display track indexes, starting from 1
            exp_tracks.get(i),
            act_tracks.get(i).map(|v| &**v),
        );
    }

    // when there are differences display exp / act
    if !diff_lines.is_empty() {
        let mut lines = Vec::new();
        lines.push(Line::new("", "Actual:", "Expected:"));
        lines.extend(diff_lines);
        print_lines(&lines);
        return unsupported_error("info is different");
    }

    Ok(())
}

fn compare_tracks(lines: &mut Vec<Line>, index: usize, act: Option<&Track>, exp: Option<&Track>) {
    let mut diff_lines = Vec::new();
    match (act, exp) {
        // tracks present on both sides
        (Some(act), Some(exp)) => {
            if !equal_codec_params_type(act, exp) {
                diff_lines.push(Line::new(
                    "TrackType",
                    get_codec_type(index, &act.codec_params),
                    get_codec_type(index, &exp.codec_params),
                ))
            }
            else {
                compare_track(&mut diff_lines, act, exp);
            }
        }
        // only act track is present
        (Some(act), None) => {
            diff_lines.push(Line::new("TrackType", get_codec_type(index, &act.codec_params), EMPTY))
        }
        // only exp track is present
        (None, Some(exp)) => {
            diff_lines.push(Line::new("TrackType", EMPTY, get_codec_type(index, &exp.codec_params)))
        }
        _ => {}
    }

    if !diff_lines.is_empty() {
        match (act, exp) {
            (Some(act), Some(exp)) => {
                if equal_codec_params_type(act, exp) {
                    lines.push(Line::new_line(
                        format!("{} {}", get_codec_type(index, &exp.codec_params), index).as_str(),
                    ));
                }
                else {
                    lines.push(Line::new_line(format!("Track {}", index).as_str()));
                }
            }
            _ => lines.push(Line::new_line(format!("Track {}", index).as_str())),
        }

        lines.extend(diff_lines);
    }
}

fn compare_track(diff_lines: &mut Vec<Line>, act: &Track, exp: &Track) {
    if act.id != exp.id {
        diff_lines.push(Line::new("Id", act.id.to_string().as_str(), exp.id.to_string().as_str()));
    }

    compare_codec_params(diff_lines, &act.codec_params, &exp.codec_params);

    if act.language != exp.language {
        diff_lines.push(Line::new("Language", to_ref(&act.language), to_ref(&exp.language)));
    }

    // compare duration, by converting to seconds and ignoring last millisecond.
    let act_duration = get_duration(act);
    let exp_duration = get_duration(exp);
    if act_duration[..act_duration.len() - 1] != exp_duration[..exp_duration.len() - 1] {
        diff_lines.push(Line::new_s("Duration".to_string(), act_duration, exp_duration));
    }
}

fn get_duration(tr: &Track) -> String {
    match (tr.time_base, tr.num_frames) {
        (Some(timebase), Some(ts)) => {
            let time = timebase.calc_time(ts);
            // Format with 3 decimal places
            format!("{}.{}", time.seconds, (time.frac * 1000.0).trunc())
        }
        _ => "None".to_string(),
    }
}

fn to_ref(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("None")
}

fn to_ref_s<T: ToString>(value: &Option<T>) -> String {
    value.as_ref().map_or("None".to_string(), |v| v.to_string())
}

fn compare_codec_params(
    diff_lines: &mut Vec<Line>,
    act: &Option<CodecParameters>,
    exp: &Option<CodecParameters>,
) {
    match (act, exp) {
        (Some(CodecParameters::Video(act)), Some(CodecParameters::Video(exp))) => {
            compare_v_params(diff_lines, act, exp);
        }
        (Some(CodecParameters::Audio(act)), Some(CodecParameters::Audio(exp))) => {
            compare_a_params(diff_lines, act, exp);
        }
        (Some(CodecParameters::Subtitle(act)), Some(CodecParameters::Subtitle(exp))) => {
            compare_s_params(diff_lines, act, exp);
        }
        _ => {}
    }
}

fn compare_v_params(
    diff_lines: &mut Vec<Line>,
    act: &VideoCodecParameters,
    exp: &VideoCodecParameters,
) {
    if act.codec != exp.codec {
        diff_lines.push(Line::new("Format", get_v_codec(act.codec), get_v_codec(exp.codec)));
    }
    if act.width != exp.width {
        diff_lines.push(Line::new_s(
            "Width".to_string(),
            to_ref_s(&act.width),
            to_ref_s(&exp.width),
        ));
    }
    if act.height != exp.height {
        diff_lines.push(Line::new_s(
            "Height".to_string(),
            to_ref_s(&act.height),
            to_ref_s(&exp.height),
        ));
    }
}

fn compare_a_params(
    diff_lines: &mut Vec<Line>,
    act: &AudioCodecParameters,
    exp: &AudioCodecParameters,
) {
    if act.codec != exp.codec {
        diff_lines.push(Line::new("Format", get_a_codec(act.codec), get_a_codec(exp.codec)));
    }
    if act.channels != exp.channels {
        diff_lines.push(Line::new_s(
            "Channels".to_string(),
            to_ref_s(&act.channels),
            to_ref_s(&exp.channels),
        ));
    }
    if act.sample_rate != exp.sample_rate {
        diff_lines.push(Line::new_s(
            "SampleRate".to_string(),
            to_ref_s(&act.sample_rate),
            to_ref_s(&exp.sample_rate),
        ));
    }
}

fn compare_s_params(
    diff_lines: &mut Vec<Line>,
    act: &SubtitleCodecParameters,
    exp: &SubtitleCodecParameters,
) {
    if act.codec != exp.codec {
        diff_lines.push(Line::new("Format", get_s_codec(act.codec), get_s_codec(exp.codec)));
    }
}

fn get_v_codec(codec: VideoCodecId) -> &'static str {
    match codec {
        CODEC_ID_MJPEG => "MJPEG",
        CODEC_ID_BINK_VIDEO => "BINK_VIDEO",
        CODEC_ID_SMACKER_VIDEO => "SMACKER_VIDEO",
        CODEC_ID_CINEPAK => "CINEPAK",
        CODEC_ID_INDEO2 => "INDEO2",
        CODEC_ID_INDEO3 => "INDEO3",
        CODEC_ID_INDEO4 => "INDEO4",
        CODEC_ID_INDEO5 => "INDEO5",
        CODEC_ID_SVQ1 => "SVQ1",
        CODEC_ID_SVQ3 => "SVQ3",
        CODEC_ID_FLV => "FLV",
        CODEC_ID_RV10 => "RV10",
        CODEC_ID_RV20 => "RV20",
        CODEC_ID_RV30 => "RV30",
        CODEC_ID_RV40 => "RV40",
        CODEC_ID_MSMPEG4V1 => "MSMPEG4V1",
        CODEC_ID_MSMPEG4V2 => "MSMPEG4V2",
        CODEC_ID_MSMPEG4V3 => "MSMPEG4V3",
        CODEC_ID_WMV1 => "WMV1",
        CODEC_ID_WMV2 => "WMV2",
        CODEC_ID_WMV3 => "WMV3",
        CODEC_ID_VP3 => "VP3",
        CODEC_ID_VP4 => "VP4",
        CODEC_ID_VP5 => "VP5",
        CODEC_ID_VP6 => "VP6",
        CODEC_ID_VP7 => "VP7",
        CODEC_ID_VP8 => "VP8",
        CODEC_ID_VP9 => "VP9",
        CODEC_ID_THEORA => "THEORA",
        CODEC_ID_AV1 => "AV1",
        CODEC_ID_MPEG1 => "MPEG1",
        CODEC_ID_MPEG2 => "MPEG2",
        CODEC_ID_MPEG4 => "MPEG4",
        CODEC_ID_H261 => "H261",
        CODEC_ID_H263 => "H263",
        CODEC_ID_H264 => "H264",
        CODEC_ID_HEVC => "HEVC",
        CODEC_ID_VVC => "VVC",
        CODEC_ID_VC1 => "VC1",
        CODEC_ID_AVS1 => "AVS1",
        CODEC_ID_AVS2 => "AVS2",
        CODEC_ID_AVS3 => "AVS3",
        other => {
            println!("info: cannot detect VideoCodecId: {}", other);
            "Unknown"
        }
    }
}

fn get_a_codec(codec: AudioCodecId) -> &'static str {
    match codec {
        CODEC_ID_PCM_S32LE => "PCM_S32LE",
        CODEC_ID_PCM_S32LE_PLANAR => "PCM_S32LE_PLANAR",
        CODEC_ID_PCM_S32BE => "PCM_S32BE",
        CODEC_ID_PCM_S32BE_PLANAR => "PCM_S32BE_PLANAR",
        CODEC_ID_PCM_S24LE => "PCM_S24LE",
        CODEC_ID_PCM_S24LE_PLANAR => "PCM_S24LE_PLANAR",
        CODEC_ID_PCM_S24BE => "PCM_S24BE",
        CODEC_ID_PCM_S24BE_PLANAR => "PCM_S24BE_PLANAR",
        CODEC_ID_PCM_S16LE => "PCM_S16LE",
        CODEC_ID_PCM_S16LE_PLANAR => "PCM_S16LE_PLANAR",
        CODEC_ID_PCM_S16BE => "PCM_S16BE",
        CODEC_ID_PCM_S16BE_PLANAR => "PCM_S16BE_PLANAR",
        CODEC_ID_PCM_S8 => "PCM_S8",
        CODEC_ID_PCM_S8_PLANAR => "PCM_S8_PLANAR",
        CODEC_ID_PCM_U32LE => "PCM_U32LE",
        CODEC_ID_PCM_U32LE_PLANAR => "PCM_U32LE_PLANAR",
        CODEC_ID_PCM_U32BE => "PCM_U32BE",
        CODEC_ID_PCM_U32BE_PLANAR => "PCM_U32BE_PLANAR",
        CODEC_ID_PCM_U24LE => "PCM_U24LE",
        CODEC_ID_PCM_U24LE_PLANAR => "PCM_U24LE_PLANAR",
        CODEC_ID_PCM_U24BE => "PCM_U24BE",
        CODEC_ID_PCM_U24BE_PLANAR => "PCM_U24BE_PLANAR",
        CODEC_ID_PCM_U16LE => "PCM_U16LE",
        CODEC_ID_PCM_U16LE_PLANAR => "PCM_U16LE_PLANAR",
        CODEC_ID_PCM_U16BE => "PCM_U16BE",
        CODEC_ID_PCM_U16BE_PLANAR => "PCM_U16BE_PLANAR",
        CODEC_ID_PCM_U8 => "PCM_U8",
        CODEC_ID_PCM_U8_PLANAR => "PCM_U8_PLANAR",
        CODEC_ID_PCM_F32LE => "PCM_F32LE",
        CODEC_ID_PCM_F32LE_PLANAR => "PCM_F32LE_PLANAR",
        CODEC_ID_PCM_F32BE => "PCM_F32BE",
        CODEC_ID_PCM_F32BE_PLANAR => "PCM_F32BE_PLANAR",
        CODEC_ID_PCM_F64LE => "PCM_F64LE",
        CODEC_ID_PCM_F64LE_PLANAR => "PCM_F64LE_PLANAR",
        CODEC_ID_PCM_F64BE => "PCM_F64BE",
        CODEC_ID_PCM_F64BE_PLANAR => "PCM_F64BE_PLANAR",
        CODEC_ID_PCM_ALAW => "PCM_ALAW",
        CODEC_ID_PCM_MULAW => "PCM_MULAW",
        CODEC_ID_ADPCM_G722 => "ADPCM_G722",
        CODEC_ID_ADPCM_G726 => "ADPCM_G726",
        CODEC_ID_ADPCM_G726LE => "ADPCM_G726LE",
        CODEC_ID_ADPCM_MS => "ADPCM_MS",
        CODEC_ID_ADPCM_IMA_WAV => "ADPCM_IMA_WAV",
        CODEC_ID_ADPCM_IMA_QT => "ADPCM_IMA_QT",
        CODEC_ID_VORBIS => "VORBIS",
        CODEC_ID_OPUS => "OPUS",
        CODEC_ID_SPEEX => "SPEEX",
        CODEC_ID_MUSEPACK => "MUSEPACK",
        CODEC_ID_MP1 => "MP1",
        CODEC_ID_MP2 => "MP2",
        CODEC_ID_MP3 => "MP3",
        CODEC_ID_AAC => "AAC",
        CODEC_ID_AC3 => "AC3",
        CODEC_ID_EAC3 => "EAC3",
        CODEC_ID_AC4 => "AC4",
        CODEC_ID_DCA => "DCA",
        CODEC_ID_ATRAC1 => "ATRAC1",
        CODEC_ID_ATRAC3 => "ATRAC3",
        CODEC_ID_ATRAC3PLUS => "ATRAC3PLUS",
        CODEC_ID_ATRAC9 => "ATRAC9",
        CODEC_ID_WMA => "WMA",
        CODEC_ID_RA10 => "RA10",
        CODEC_ID_RA20 => "RA20",
        CODEC_ID_SIPR => "SIPR",
        CODEC_ID_COOK => "COOK",
        CODEC_ID_SBC => "SBC",
        CODEC_ID_APTX => "APTX",
        CODEC_ID_APTX_HD => "APTX_HD",
        CODEC_ID_LDAC => "LDAC",
        CODEC_ID_BINK_AUDIO => "BINK_AUDIO",
        CODEC_ID_SMACKER_AUDIO => "SMACKER_AUDIO",
        CODEC_ID_FLAC => "FLAC",
        CODEC_ID_WAVPACK => "WAVPACK",
        CODEC_ID_MONKEYS_AUDIO => "MONKEYS_AUDIO",
        CODEC_ID_ALAC => "ALAC",
        CODEC_ID_TTA => "TTA",
        CODEC_ID_RALF => "RALF",
        CODEC_ID_TRUEHD => "TRUEHD",
        other => {
            println!("info: cannot detect AudioCodecId: {}", other);
            "Unknown"
        }
    }
}

fn get_s_codec(codec: SubtitleCodecId) -> &'static str {
    match codec {
        CODEC_ID_TEXT_UTF8 => "TEXT_UTF8",
        CODEC_ID_SSA => "SSA",
        CODEC_ID_ASS => "ASS",
        CODEC_ID_SAMI => "SAMI",
        CODEC_ID_SRT => "SRT",
        CODEC_ID_WEBVTT => "WEBVTT",
        CODEC_ID_DVBSUB => "DVBSUB",
        CODEC_ID_HDMV_TEXTST => "HDMV_TEXTST",
        CODEC_ID_MOV_TEXT => "MOV_TEXT",
        CODEC_ID_BMP => "BMP",
        CODEC_ID_VOBSUB => "VOBSUB",
        CODEC_ID_HDMV_PGS => "HDMV_PGS",
        CODEC_ID_KATE => "KATE",
        other => {
            println!("info: cannot detect SubtitleCodecId: {}", other);
            "Unknown"
        }
    }
}

fn equal_codec_params_type(act: &Track, exp: &Track) -> bool {
    matches!(
        (&act.codec_params, &exp.codec_params),
        (Some(CodecParameters::Video(_)), Some(CodecParameters::Video(_)))
            | (Some(CodecParameters::Audio(_)), Some(CodecParameters::Audio(_)))
            | (Some(CodecParameters::Subtitle(_)), Some(CodecParameters::Subtitle(_)))
            | (None, None)
    )
}

fn get_codec_type(index: usize, codec_params: &Option<CodecParameters>) -> &str {
    match codec_params {
        Some(CodecParameters::Video(_)) => "Video",
        Some(CodecParameters::Audio(_)) => "Audio",
        Some(CodecParameters::Subtitle(_)) => "Text",
        _ => {
            println!("info: cannot detect CodecParameters type, for track_id: {}", index);
            "Unknown"
        }
    }
}

fn print_lines(lines: &Vec<Line>) {
    for line in lines {
        if line.title.is_empty() {
            println!("                {:<20}\t{:<20}", line.act, line.exp);
        }
        else if line.exp.is_empty() && line.act.is_empty() {
            println!("{}", line.title);
        }
        else {
            println!("{:>14}: {:<20}\t{:<20}", line.title, line.act, line.exp);
        }
    }
}
