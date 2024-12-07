// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::borrow::Cow;
use std::io::Write;
use std::path::Path;

use lazy_static::lazy_static;
use symphonia::core::codecs::{CodecInfo, CodecParameters, CodecProfile};
use symphonia::core::formats::{FormatReader, Track, TrackFlags};
use symphonia::core::meta::{
    Chapter, ChapterGroup, ChapterGroupItem, ColorMode, ColorModel, ContentAdvisory,
    MetadataRevision, StandardTag, Tag, Visual,
};
use symphonia::core::units::{Time, TimeBase};
use symphonia::core::util::text;

/// The minimum padding for tag keys.
const MIN_PAD: usize = 20;
/// The maximum padding for tag keys.
const MAX_PAD: usize = 40;

pub fn print_format(path: &Path, format: &mut Box<dyn FormatReader>) {
    println!("+ {}", path.display());

    let format_info = format.format_info();

    print_blank();
    print_header("Container");
    print_pair(
        "Format Name:",
        &format!("{} ({})", format_info.long_name, format_info.short_name),
        Bullet::None,
        1,
    );
    print_pair("Format ID:", &format_info.format, Bullet::None, 1);

    print_tracks(format.tracks());

    // Consume all metadata revisions up-to and including the latest.
    loop {
        if let Some(revision) = format.metadata().current() {
            print_tags(revision.tags());
            print_visuals(revision.visuals());
        }

        if format.metadata().is_latest() {
            break;
        }

        format.metadata().pop();
    }

    print_chapters(format.chapters());
    println!(":");
    println!();
}

pub fn print_update(rev: &MetadataRevision) {
    print_tags(rev.tags());
    print_visuals(rev.visuals());
    println!(":");
    println!();
}

pub fn print_tracks(tracks: &[Track]) {
    if !tracks.is_empty() {
        // Default codec registry.
        let reg = symphonia::default::get_codecs();

        print_blank();
        print_header("Tracks");

        for (idx, track) in tracks.iter().enumerate() {
            match &track.codec_params {
                Some(CodecParameters::Audio(params)) => {
                    let codec_info = reg.get_audio_decoder(params.codec).map(|d| &d.codec.info);

                    print_pair("Track Type:", &"Audio", Bullet::Num(idx + 1), 1);
                    print_pair("Codec Name:", &fmt_codec_name(codec_info), Bullet::None, 1);
                    print_pair("Codec ID:", &params.codec, Bullet::None, 1);

                    if let Some(profile) = params.profile {
                        print_pair(
                            "Profile:",
                            &fmt_codec_profile(profile, codec_info),
                            Bullet::None,
                            1,
                        );
                    }
                    if let Some(rate) = params.sample_rate {
                        print_pair("Sample Rate:", &rate, Bullet::None, 1);
                    }
                    if let Some(fmt) = params.sample_format {
                        print_pair("Sample Format:", &format!("{:?}", fmt), Bullet::None, 1);
                    }
                    if let Some(bits_per_sample) = params.bits_per_sample {
                        print_pair("Bits per Sample:", &bits_per_sample, Bullet::None, 1);
                    }
                    if let Some(channels) = &params.channels {
                        print_pair("Channel(s):", &channels.count(), Bullet::None, 1);
                        print_pair("Channel Map:", &channels, Bullet::None, 1);
                    }
                }
                Some(CodecParameters::Video(params)) => {
                    let codec_info = reg.get_video_decoder(params.codec).map(|d| &d.codec.info);

                    print_pair("Track Type:", &"Video", Bullet::Num(idx + 1), 1);
                    print_pair("Codec Name:", &fmt_codec_name(codec_info), Bullet::None, 1);
                    print_pair("Codec ID:", &params.codec, Bullet::None, 1);

                    if let Some(profile) = params.profile {
                        print_pair(
                            "Profile:",
                            &fmt_codec_profile(profile, codec_info),
                            Bullet::None,
                            1,
                        );
                    }
                    if let Some(level) = params.level {
                        print_pair("Level:", &level, Bullet::None, 1);
                    }
                    if let Some(width) = params.width {
                        print_pair("Width:", &width, Bullet::None, 1);
                    }
                    if let Some(height) = params.height {
                        print_pair("Height:", &height, Bullet::None, 1);
                    }
                }
                Some(CodecParameters::Subtitle(params)) => {
                    let codec_name = fmt_codec_name(
                        reg.get_subtitle_decoder(params.codec).map(|d| &d.codec.info),
                    );

                    print_pair("Track Type:", &"Subtitle", Bullet::Num(idx + 1), 1);
                    print_pair("Codec Name:", &codec_name, Bullet::None, 1);
                    print_pair("Codec ID:", &params.codec, Bullet::None, 1);
                }
                _ => {
                    print_pair("Track Type:", &"*Unsupported*", Bullet::Num(idx + 1), 1);
                }
            }

            if let Some(tb) = track.time_base {
                print_pair("Time Base:", &tb, Bullet::None, 1);
            }

            if track.start_ts > 0 {
                if let Some(tb) = track.time_base {
                    print_pair(
                        "Start Time:",
                        &format!("{} ({})", fmt_ts(track.start_ts, tb), track.start_ts),
                        Bullet::None,
                        1,
                    );
                }
                else {
                    print_pair("Start Time:", &track.start_ts, Bullet::None, 1);
                }
            }

            if let Some(num_frames) = track.num_frames {
                if let Some(tb) = track.time_base {
                    print_pair(
                        "Duration:",
                        &format!("{} ({})", fmt_ts(num_frames, tb), num_frames),
                        Bullet::None,
                        1,
                    );
                }
                else {
                    print_pair("Frames:", &num_frames, Bullet::None, 1);
                }
            }

            if let Some(delay) = track.delay {
                print_pair("Encoder Delay:", &delay, Bullet::None, 1);
            }

            if let Some(padding) = track.padding {
                print_pair("Encoder Padding:", &padding, Bullet::None, 1);
            }

            if let Some(language) = &track.language {
                print_pair("Language:", &language, Bullet::None, 1);
            }

            if !track.flags.is_empty() {
                for (i, flag) in track.flags.iter().enumerate() {
                    let name = match flag {
                        TrackFlags::DEFAULT => "Default",
                        TrackFlags::FORCED => "Forced",
                        TrackFlags::ORIGINAL_LANGUAGE => "Original Language",
                        TrackFlags::COMMENTARY => "Commentary",
                        TrackFlags::HEARING_IMPAIRED => "Hearing Impaired",
                        TrackFlags::VISUALLY_IMPAIRED => "Visually Impaired",
                        TrackFlags::TEXT_DESCRIPTIONS => "Text description",
                        _ => "*Unknown*",
                    };
                    print_pair(if i == 0 { "Flags:" } else { "" }, &name, Bullet::None, 1);
                }
            }
        }
    }
}

pub fn print_chapters(chapters: Option<&ChapterGroup>) {
    if let Some(chapters) = chapters {
        print_blank();
        print_header("Chapters");

        fn print_chapter(chap: &Chapter, idx: usize, depth: usize) {
            // Chapter bounds.
            print_pair("Start Time:", &fmt_time(chap.start_time), Bullet::Num(idx), depth);
            if let Some(end_time) = chap.end_time {
                print_pair("End Time:", &fmt_time(end_time), Bullet::None, depth);
            }

            // Chapter tags.
            if !chap.tags.is_empty() {
                print_one("Tags:", Bullet::None, depth);
                let pad = optimal_tag_key_pad(&chap.tags, MIN_PAD - 5, MAX_PAD);

                for (i, tag) in chap.tags.iter().enumerate() {
                    print_tag(tag, Bullet::Num(i + 1), pad, depth + 1);
                }
            }
        }

        fn print_chapter_group(group: &ChapterGroup, idx: usize, depth: usize) {
            print_one("Chapter Group:", Bullet::Num(idx), depth);

            // Chapter group tags.
            if !group.tags.is_empty() {
                print_one("Tags:", Bullet::None, depth);
                let pad = optimal_tag_key_pad(&group.tags, MIN_PAD - 5, MAX_PAD);

                for (i, tag) in group.tags.iter().enumerate() {
                    print_tag(tag, Bullet::Num(i + 1), pad, depth + 1);
                }
            }

            // Chapter group items.
            print_one("Items:", Bullet::None, depth);
            for (i, item) in group.items.iter().enumerate() {
                match item {
                    ChapterGroupItem::Group(group) => print_chapter_group(group, i + 1, depth + 1),
                    ChapterGroupItem::Chapter(chap) => print_chapter(chap, i + 1, depth + 1),
                }
            }
        }

        // Start recursion.
        print_chapter_group(chapters, 1, 1);
    }
}

pub fn print_tags(tags: &[Tag]) {
    if !tags.is_empty() {
        print_blank();
        print_header("Tags");

        let mut idx = 1;

        // Find maximum tag key string length, then constrain it to reasonable limits.
        let pad = optimal_tag_key_pad(tags, MIN_PAD, MAX_PAD);

        // Print tags with a standard tag first, these are the most common tags.
        for tag in tags.iter().filter(|tag| tag.has_std_tag()) {
            print_tag(tag, Bullet::Num(idx), pad, 1);
            idx += 1;
        }

        // Print the remaining tags with keys truncated to the optimal key length.
        for tag in tags.iter().filter(|tag| !tag.has_std_tag()) {
            print_tag(tag, Bullet::Num(idx), pad, 1);
            idx += 1;
        }
    }
}

pub fn print_tag(tag: &Tag, bullet: Bullet, pad: usize, depth: usize) {
    let formatted = fmt_tag(tag);
    print_pair_custom(&formatted.key, &formatted.value, bullet, pad, depth);

    // Sub-fields.
    if let Some(fields) = &tag.raw.sub_fields {
        if !fields.is_empty() {
            print_one("Sub-fields:", Bullet::None, depth);
            for (i, sub_field) in fields.iter().enumerate() {
                print_pair_custom(
                    &sub_field.field,
                    &sub_field.value.to_string(),
                    Bullet::Num(i + 1),
                    pad - 5,
                    depth + 1,
                );
            }
        }
    }
}

pub fn print_visuals(visuals: &[Visual]) {
    if !visuals.is_empty() {
        print_blank();
        print_header("Visuals");

        for (idx, visual) in visuals.iter().enumerate() {
            if let Some(usage) = visual.usage {
                print_pair("Usage:", &format!("{:?}", usage), Bullet::Num(idx + 1), 1);
            }
            if let Some(media_type) = &visual.media_type {
                let bullet =
                    if visual.usage.is_some() { Bullet::None } else { Bullet::Num(idx + 1) };
                print_pair("Media Type:", media_type, bullet, 1);
            }
            if let Some(dimensions) = visual.dimensions {
                print_pair(
                    "Dimensions:",
                    &format!("{} x {} px", dimensions.width, dimensions.height),
                    Bullet::None,
                    1,
                );
            }

            match visual.color_mode {
                Some(ColorMode::Direct(model)) => {
                    print_pair("Color Mode:", &"Direct", Bullet::None, 1);
                    print_pair("Color Model:", &fmt_color_model(model), Bullet::None, 1);
                    print_pair("Bits/Pixel:", &model.bits_per_pixel(), Bullet::None, 1);
                }
                Some(ColorMode::Indexed(palette)) => {
                    print_pair("Color Mode:", &"Indexed", Bullet::None, 1);
                    print_pair("Bits/Pixel:", &palette.bits_per_pixel, Bullet::None, 1);
                    print_pair(
                        "Color Model:",
                        &fmt_color_model(palette.color_model),
                        Bullet::None,
                        1,
                    );
                }
                _ => (),
            }

            print_pair("Size:", &fmt_size(visual.data.len()), Bullet::None, 1);

            // Print out tags similar to how regular tags are printed.
            if !visual.tags.is_empty() {
                print_one("Tags:", Bullet::None, 1);

                let pad = optimal_tag_key_pad(&visual.tags, MIN_PAD - 5, MAX_PAD);

                for (tidx, tag) in visual.tags.iter().enumerate() {
                    print_tag(tag, Bullet::Num(tidx + 1), pad, 2);
                }
            }
        }
    }
}

/// A list bullet.
#[allow(dead_code)]
pub enum Bullet {
    /// No bullet.
    None,
    /// A numbered bullet.
    Num(usize),
    /// A custom character.
    Char(char),
}

impl std::fmt::Display for Bullet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The bullet must occupy 4 characters.
        match self {
            Bullet::None => write!(f, "    "),
            Bullet::Num(num) => write!(f, "[{:0>2}]", num),
            Bullet::Char(ch) => write!(f, "   {}", ch),
        }
    }
}

/// Print one value as a plain, numbered, or bulleted list item in a hierarchical list.
pub fn print_one(value: &str, bullet: Bullet, depth: usize) {
    let indent = 5 * depth;
    // The format is: "|<INDENT><BULLET> <VALUE>"
    println!("|{:indent$}{} {}", "", bullet, value)
}

/// Print a key-value pair as a plain, numbered, or bulleted list item in a hierarchical list.
///
/// The key padding may be customized with `pad`.
pub fn print_pair_custom(key: &str, value: &str, bullet: Bullet, pad: usize, depth: usize) {
    let indent = 5 * depth;
    let key = pad_key(key, pad);

    // The format is: "|<INDENT><BULLET> <KEY> "
    print!("|{:indent$}{} {} ", "", bullet, key);

    print_pair_value(value, indent + key.len() + 4 + 2);
}

/// Print a key-value pair as a plain, numbered, or bulleted list item in a hierarchical list with
/// default key padding.
pub fn print_pair<T>(key: &str, value: &T, bullet: Bullet, depth: usize)
where
    T: std::fmt::Display,
{
    print_pair_custom(key, &value.to_string(), bullet, MIN_PAD, depth)
}

#[inline(never)]
pub fn print_pair_value(value: &str, lead: usize) {
    if !value.is_empty() {
        // Print multi-line values with wrapping.
        //
        // NOTE: lines() does not split on orphan carriage returns ('\r') if a line feed ('\n') does
        // not follow. These orphan carriage returns will appear as a C0 control character.
        for (i, line) in value.lines().enumerate() {
            let mut chars = line.chars();

            for (j, seg) in (0..)
                .map(|_| {
                    // Try to wrap at the first whitespace character after 60 characters, or force
                    // wrapping at 80 charaters. C0 control characters will be replaced with their
                    // respective graphical symbols, while C1 control characters will be silently
                    // removed.
                    chars
                        .by_ref()
                        .filter(text::filter::not_c1_control)
                        .enumerate()
                        .take_while(|(i, c)| *i <= 60 || *i <= 80 && !c.is_whitespace())
                        .map(|(_, c)| {
                            if text::filter::c0_control(&c) {
                                char::from_u32(0x2400 + c as u32).unwrap()
                            }
                            else {
                                c
                            }
                        })
                        .collect::<String>()
                })
                .take_while(|s| !s.is_empty())
                .enumerate()
            {
                // Print new output line prefix.
                if i > 0 || j > 0 {
                    print!("|{:lead$}", "");
                }
                // Print line-wrapping character if this is a line-wrap.
                if j > 0 {
                    print!("\u{21aa} ")
                }
                // Print sub-string.
                println!("{}", seg)
            }
        }
    }
    else {
        println!();
    }
}

/// Print a list header.
pub fn print_header(title: &str) {
    println!("| // {} //", title)
}

/// Print a blank list line.
pub fn print_blank() {
    println!("|")
}

pub fn print_progress(ts: u64, dur: Option<u64>, tb: Option<TimeBase>) {
    // Get a string slice containing a progress bar.
    fn progress_bar(ts: u64, dur: u64) -> &'static str {
        const NUM_STEPS: usize = 60;

        lazy_static! {
            static ref PROGRESS_BAR: Vec<String> = {
                (0..NUM_STEPS + 1).map(|i| format!("[{:<60}]", str::repeat("■", i))).collect()
            };
        }

        let i = (NUM_STEPS as u64)
            .saturating_mul(ts)
            .checked_div(dur)
            .unwrap_or(0)
            .clamp(0, NUM_STEPS as u64);

        &PROGRESS_BAR[i as usize]
    }

    // Multiple print! calls would need to be made to print the progress, so instead, only lock
    // stdout once and use write! rather then print!.
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    if let Some(tb) = tb {
        let t = tb.calc_time(ts);

        let hours = t.seconds / (60 * 60);
        let mins = (t.seconds % (60 * 60)) / 60;
        let secs = f64::from((t.seconds % 60) as u32) + t.frac;

        write!(output, "\r\u{25b6}\u{fe0f}  {}:{:0>2}:{:0>4.1}", hours, mins, secs).unwrap();

        if let Some(dur) = dur {
            let d = tb.calc_time(dur.saturating_sub(ts));

            let hours = d.seconds / (60 * 60);
            let mins = (d.seconds % (60 * 60)) / 60;
            let secs = f64::from((d.seconds % 60) as u32) + d.frac;

            write!(output, " {} -{}:{:0>2}:{:0>4.1}", progress_bar(ts, dur), hours, mins, secs)
                .unwrap();
        }
    }
    else {
        write!(output, "\r\u{25b6}\u{fe0f}  {}", ts).unwrap();
    }

    // This extra space is a workaround for Konsole to correctly erase the previous line.
    write!(output, " ").unwrap();

    // Flush immediately since stdout is buffered.
    output.flush().unwrap();
}

/// Calculate the appropriate length for tag key padding.
fn optimal_tag_key_pad(tags: &[Tag], min: usize, max: usize) -> usize {
    tags.iter().map(|tag| fmt_tag(tag).key.chars().count()).max().unwrap_or(min).clamp(min, max)
}

/// Pad a key.
fn pad_key(key: &str, pad: usize) -> String {
    if key.len() <= pad {
        format!("{:<pad$}", key)
    }
    else {
        // Key length too large.
        format!("{:.<pad$}", key.split_at(pad - 2).0)
    }
}

fn fmt_color_model(model: ColorModel) -> String {
    match model {
        ColorModel::Y(b) => format!("Y{b}"),
        ColorModel::YA(b) => format!("Y{b}A{b}"),
        ColorModel::RGB(b) => format!("R{b}G{b}B{b}"),
        ColorModel::RGBA(b) => format!("R{b}G{b}B{b}A{b}"),
        ColorModel::CMYK(b) => format!("C{b}M{b}Y{b}K{b}"),
        _ => "*Unknown*".to_string(),
    }
}

fn fmt_codec_name(info: Option<&CodecInfo>) -> String {
    match info {
        Some(info) => format!("{} ({})", info.long_name, info.short_name),
        None => "*Unknown*".to_string(),
    }
}

fn fmt_codec_profile(profile: CodecProfile, info: Option<&CodecInfo>) -> String {
    // Try to find the codec profile information.
    let profile_info = info
        .map(|codec_info| codec_info.profiles)
        .and_then(|profiles| profiles.iter().find(|profile_info| profile_info.profile == profile));

    match profile_info {
        Some(info) => format!("{} ({}) [{}]", info.long_name, info.short_name, profile.get()),
        None => format!("{}", profile.get()),
    }
}

fn fmt_size(size: usize) -> String {
    // < 1 KiB
    if size < 1 << 10 {
        // Show in Bytes
        format!("{} B", size)
    }
    // < 1 MiB
    else if size < 1 << 20 {
        // Show in Kibibytes
        format!("{:.1} KiB ({} B)", (size as f64) / 1024.0, size)
    }
    // < 1 GiB
    else if size < 1 << 30 {
        // Show in Mebibytes
        format!("{:.1} MiB ({} B)", ((size >> 10) as f64) / 1024.0, size)
    }
    // >= 1 GiB
    else {
        // Show in Gibibytes
        format!("{:.1} GiB ({} B)", ((size >> 20) as f64) / 1024.0, size)
    }
}

fn fmt_ts(ts: u64, tb: TimeBase) -> String {
    let time = tb.calc_time(ts);
    fmt_time(time)
}

fn fmt_time(time: Time) -> String {
    let hours = time.seconds / (60 * 60);
    let mins = (time.seconds % (60 * 60)) / 60;
    let secs = f64::from((time.seconds % 60) as u32) + time.frac;

    format!("{}:{:0>2}:{:0>6.3}", hours, mins, secs)
}

struct FormattedTag<'a> {
    key: Cow<'a, str>,
    value: Cow<'a, str>,
}

impl<'a> FormattedTag<'a> {
    fn new<V>(key: &'a str, value: V) -> Self
    where
        V: Into<Cow<'a, str>>,
    {
        FormattedTag { key: Cow::from(key), value: value.into() }
    }
}

fn fmt_tag(tag: &Tag) -> FormattedTag<'_> {
    let std_tag = match &tag.std {
        Some(std) => std,
        // No standard tag. Format raw tag key and value instead.
        _ => return FormattedTag::new(&tag.raw.key, format!("{}", tag.raw.value)),
    };

    match std_tag {
        StandardTag::AccurateRipCount(v) => FormattedTag::new("AccurateRip Count", &**v),
        StandardTag::AccurateRipCountAllOffsets(v) => {
            FormattedTag::new("AccurateRip Count All Offsets", &**v)
        }
        StandardTag::AccurateRipCountWithOffset(v) => {
            FormattedTag::new("AccurateRip Count With Offset", &**v)
        }
        StandardTag::AccurateRipCrc(v) => FormattedTag::new("AccurateRip CRC", &**v),
        StandardTag::AccurateRipDiscId(v) => FormattedTag::new("AccurateRip Disc ID", &**v),
        StandardTag::AccurateRipId(v) => FormattedTag::new("AccurateRip ID", &**v),
        StandardTag::AccurateRipOffset(v) => FormattedTag::new("AccurateRip Offset", &**v),
        StandardTag::AccurateRipResult(v) => FormattedTag::new("AccurateRip Result", &**v),
        StandardTag::AccurateRipTotal(v) => FormattedTag::new("AccurateRip Total", &**v),
        StandardTag::AcoustIdFingerprint(v) => FormattedTag::new("AcoustId Fingerprint", &**v),
        StandardTag::AcoustIdId(v) => FormattedTag::new("AcoustId ID", &**v),
        StandardTag::Album(v) => FormattedTag::new("Album", &**v),
        StandardTag::AlbumArtist(v) => FormattedTag::new("Album Artist", &**v),
        StandardTag::Arranger(v) => FormattedTag::new("Arranger", &**v),
        StandardTag::Artist(v) => FormattedTag::new("Artist", &**v),
        StandardTag::Author(v) => FormattedTag::new("Author", &**v),
        StandardTag::Bpm(v) => FormattedTag::new("BPM", v.to_string()),
        StandardTag::CdToc(v) => FormattedTag::new("CD Table of Contents", &**v),
        StandardTag::Comment(v) => FormattedTag::new("Comment", &**v),
        StandardTag::CompilationFlag(v) => {
            FormattedTag::new("Is Compilation", if *v { "<Yes>" } else { "<No>" })
        }
        StandardTag::Composer(v) => FormattedTag::new("Composer", &**v),
        StandardTag::Conductor(v) => FormattedTag::new("Conductor", &**v),
        StandardTag::ContentAdvisory(v) => FormattedTag::new(
            "Content Advisory",
            match v {
                ContentAdvisory::None => "None",
                ContentAdvisory::Explicit => "Explicit",
                ContentAdvisory::Censored => "Censored",
            },
        ),
        StandardTag::Copyright(v) => FormattedTag::new("Copyright", &**v),
        StandardTag::CueToolsDbDiscConfidence(v) => {
            FormattedTag::new("CueTools DB Disc Confidence", &**v)
        }
        StandardTag::CueToolsDbTrackConfidence(v) => {
            FormattedTag::new("CueTools DB Track Confidence", &**v)
        }
        StandardTag::Date(v) => FormattedTag::new("Date", &**v),
        StandardTag::Description(v) => FormattedTag::new("Description", &**v),
        StandardTag::DiscNumber(v) => FormattedTag::new("Disc Number", v.to_string()),
        StandardTag::DiscSubtitle(v) => FormattedTag::new("Disc Subtitle", &**v),
        StandardTag::DiscTotal(v) => FormattedTag::new("Disc Total", v.to_string()),
        StandardTag::EncodedBy(v) => FormattedTag::new("Encoded By", &**v),
        StandardTag::Encoder(v) => FormattedTag::new("Encoder", &**v),
        StandardTag::EncoderSettings(v) => FormattedTag::new("Encoder Settings", &**v),
        StandardTag::EncodingDate(v) => FormattedTag::new("Encoding Date", &**v),
        StandardTag::Engineer(v) => FormattedTag::new("Engineer", &**v),
        StandardTag::Ensemble(v) => FormattedTag::new("Ensemble", &**v),
        StandardTag::Genre(v) => FormattedTag::new("Genre", &**v),
        StandardTag::Grouping(v) => FormattedTag::new("Grouping", &**v),
        StandardTag::IdentAsin(v) => FormattedTag::new("ASIN", &**v),
        StandardTag::IdentBarcode(v) => FormattedTag::new("Barcode", &**v),
        StandardTag::IdentCatalogNumber(v) => FormattedTag::new("Catalog Number", &**v),
        StandardTag::IdentEanUpn(v) => FormattedTag::new("EAN/UPN", &**v),
        StandardTag::IdentIsbn(v) => FormattedTag::new("ISBN", &**v),
        StandardTag::IdentIsrc(v) => FormattedTag::new("ISRC", &**v),
        StandardTag::IdentPn(v) => FormattedTag::new("PN", &**v),
        StandardTag::IdentPodcast(v) => FormattedTag::new("Podcast", &**v),
        StandardTag::IdentUpc(v) => FormattedTag::new("UPC", &**v),
        StandardTag::IndexNumber(v) => FormattedTag::new("Index Number", v.to_string()),
        StandardTag::InitialKey(v) => FormattedTag::new("Initial Key", &**v),
        StandardTag::InternetRadioName(v) => FormattedTag::new("Internet Radio Name", &**v),
        StandardTag::InternetRadioOwner(v) => FormattedTag::new("Internet Radio Owner", &**v),
        StandardTag::Label(v) => FormattedTag::new("Label", &**v),
        StandardTag::LabelCode(v) => FormattedTag::new("Label Code", &**v),
        StandardTag::Language(v) => FormattedTag::new("Language", &**v),
        StandardTag::License(v) => FormattedTag::new("License", &**v),
        StandardTag::Lyricist(v) => FormattedTag::new("Lyricist", &**v),
        StandardTag::Lyrics(v) => FormattedTag::new("Lyrics", &**v),
        StandardTag::MediaFormat(v) => FormattedTag::new("Media Format", &**v),
        StandardTag::MixDj(v) => FormattedTag::new("Mix DJ", &**v),
        StandardTag::MixEngineer(v) => FormattedTag::new("Mix Engineer", &**v),
        StandardTag::Mood(v) => FormattedTag::new("Mood", &**v),
        StandardTag::MovementName(v) => FormattedTag::new("Movement Name", &**v),
        StandardTag::MovementNumber(v) => FormattedTag::new("Movement Number", v.to_string()),
        StandardTag::MovementTotal(v) => FormattedTag::new("Movement Total", v.to_string()),
        StandardTag::Mp3GainAlbumMinMax(v) => FormattedTag::new("Mp3Gain Album MinMax", &**v),
        StandardTag::Mp3GainMinMax(v) => FormattedTag::new("Mp3Gain MinMax", &**v),
        StandardTag::Mp3GainUndo(v) => FormattedTag::new("Mp3Gain Undo", &**v),
        StandardTag::MusicBrainzAlbumArtistId(v) => {
            FormattedTag::new("MusicBrainz Album Artist ID", &**v)
        }
        StandardTag::MusicBrainzAlbumId(v) => FormattedTag::new("MusicBrainz Album ID", &**v),
        StandardTag::MusicBrainzArtistId(v) => FormattedTag::new("MusicBrainz Artist ID", &**v),
        StandardTag::MusicBrainzDiscId(v) => FormattedTag::new("MusicBrainz Disc ID", &**v),
        StandardTag::MusicBrainzGenreId(v) => FormattedTag::new("MusicBrainz Genre ID", &**v),
        StandardTag::MusicBrainzLabelId(v) => FormattedTag::new("MusicBrainz Label ID", &**v),
        StandardTag::MusicBrainzOriginalAlbumId(v) => {
            FormattedTag::new("MusicBrainz Original Album ID", &**v)
        }
        StandardTag::MusicBrainzOriginalArtistId(v) => {
            FormattedTag::new("MusicBrainz Original Artist ID", &**v)
        }
        StandardTag::MusicBrainzRecordingId(v) => {
            FormattedTag::new("MusicBrainz Recording ID", &**v)
        }
        StandardTag::MusicBrainzReleaseGroupId(v) => {
            FormattedTag::new("MusicBrainz Release Group ID", &**v)
        }
        StandardTag::MusicBrainzReleaseStatus(v) => {
            FormattedTag::new("MusicBrainz Release Status", &**v)
        }
        StandardTag::MusicBrainzReleaseTrackId(v) => {
            FormattedTag::new("MusicBrainz Release Track ID", &**v)
        }
        StandardTag::MusicBrainzReleaseType(v) => {
            FormattedTag::new("MusicBrainz Release Type", &**v)
        }
        StandardTag::MusicBrainzTrackId(v) => FormattedTag::new("MusicBrainz Track ID", &**v),
        StandardTag::MusicBrainzTrmId(v) => FormattedTag::new("MusicBrainz TRM ID", &**v),
        StandardTag::MusicBrainzWorkId(v) => FormattedTag::new("MusicBrainz Work ID", &**v),
        StandardTag::Narrator(v) => FormattedTag::new("Narrator", &**v),
        StandardTag::Opus(v) => FormattedTag::new("Opus", &**v),
        StandardTag::OriginalAlbum(v) => FormattedTag::new("Original Album", &**v),
        StandardTag::OriginalArtist(v) => FormattedTag::new("Original Artist", &**v),
        StandardTag::OriginalDate(v) => FormattedTag::new("Original Date", &**v),
        StandardTag::OriginalFile(v) => FormattedTag::new("Original File", &**v),
        StandardTag::OriginalWriter(v) => FormattedTag::new("Original Writer", &**v),
        StandardTag::OriginalYear(v) => FormattedTag::new("Original Year", v.to_string()),
        StandardTag::Owner(v) => FormattedTag::new("Owner", &**v),
        StandardTag::Part(v) => FormattedTag::new("Part", &**v),
        StandardTag::PartNumber(v) => FormattedTag::new("Part", v.to_string()),
        StandardTag::PartTotal(v) => FormattedTag::new("Part Total", v.to_string()),
        StandardTag::Performer(v) => FormattedTag::new("Performer", &**v),
        StandardTag::PlayCounter(v) => FormattedTag::new("Play Counter", v.to_string()),
        StandardTag::PodcastCategory(v) => FormattedTag::new("Podcast Category", &**v),
        StandardTag::PodcastDescription(v) => FormattedTag::new("Podcast Description", &**v),
        StandardTag::PodcastFlag(v) => {
            FormattedTag::new("Is Podcast", if *v { "<Yes>" } else { "<No>" })
        }
        StandardTag::PodcastKeywords(v) => FormattedTag::new("Podcast Keywords", &**v),
        StandardTag::Producer(v) => FormattedTag::new("Producer", &**v),
        StandardTag::ProductionCopyright(v) => FormattedTag::new("Production Copyright", &**v),
        StandardTag::PurchaseDate(v) => FormattedTag::new("Purchase Date", &**v),
        StandardTag::Rating(v) => FormattedTag::new("Rating", &**v),
        StandardTag::RecordingDate(v) => FormattedTag::new("Recording Date", &**v),
        StandardTag::RecordingLocation(v) => FormattedTag::new("Recording Location", &**v),
        StandardTag::RecordingTime(v) => FormattedTag::new("Recording Time", &**v),
        StandardTag::ReleaseCountry(v) => FormattedTag::new("Release Country", &**v),
        StandardTag::ReleaseDate(v) => FormattedTag::new("Release Date", &**v),
        StandardTag::Remixer(v) => FormattedTag::new("Remixer", &**v),
        StandardTag::ReplayGainAlbumGain(v) => FormattedTag::new("ReplayGain Album Gain", &**v),
        StandardTag::ReplayGainAlbumPeak(v) => FormattedTag::new("ReplayGain Album Peak", &**v),
        StandardTag::ReplayGainAlbumRange(v) => FormattedTag::new("ReplayGain Album Range", &**v),
        StandardTag::ReplayGainReferenceLoudness(v) => {
            FormattedTag::new("ReplayGain Reference Loudness", &**v)
        }
        StandardTag::ReplayGainTrackGain(v) => FormattedTag::new("ReplayGain Track Gain", &**v),
        StandardTag::ReplayGainTrackPeak(v) => FormattedTag::new("ReplayGain Track Peak", &**v),
        StandardTag::ReplayGainTrackRange(v) => FormattedTag::new("ReplayGain Track Range", &**v),
        StandardTag::Script(v) => FormattedTag::new("Script", &**v),
        StandardTag::Soloist(v) => FormattedTag::new("Soloist", &**v),
        StandardTag::SortAlbum(v) => FormattedTag::new("Album (Sort Order)", &**v),
        StandardTag::SortAlbumArtist(v) => FormattedTag::new("Album Artist (Sort Order)", &**v),
        StandardTag::SortArtist(v) => FormattedTag::new("Artist (Sort Order)", &**v),
        StandardTag::SortComposer(v) => FormattedTag::new("Composer (Sort Order)", &**v),
        StandardTag::SortTrackTitle(v) => FormattedTag::new("Track Title (Sort Order)", &**v),
        StandardTag::SortTvShowTitle(v) => FormattedTag::new("TV Show Title (Sort Order)", &**v),
        StandardTag::TaggingDate(v) => FormattedTag::new("Tagging Date", &**v),
        StandardTag::TermsOfUse(v) => FormattedTag::new("Terms of Use", &**v),
        StandardTag::TrackNumber(v) => FormattedTag::new("Track Number", v.to_string()),
        StandardTag::TrackSubtitle(v) => FormattedTag::new("Track Subtitle", &**v),
        StandardTag::TrackTitle(v) => FormattedTag::new("Track Title", &**v),
        StandardTag::TrackTotal(v) => FormattedTag::new("Track Total", v.to_string()),
        StandardTag::TvEpisode(v) => FormattedTag::new("TV Episode Number", v.to_string()),
        StandardTag::TvEpisodeTitle(v) => FormattedTag::new("TV Episode Title", &**v),
        StandardTag::TvNetwork(v) => FormattedTag::new("TV Network", &**v),
        StandardTag::TvSeason(v) => FormattedTag::new("TV Season", v.to_string()),
        StandardTag::TvShowTitle(v) => FormattedTag::new("TV Show Title", &**v),
        StandardTag::Url(v) => FormattedTag::new("URL", &**v),
        StandardTag::UrlArtist(v) => FormattedTag::new("Artist URL", &**v),
        StandardTag::UrlCopyright(v) => FormattedTag::new("Copyright URL", &**v),
        StandardTag::UrlInternetRadio(v) => FormattedTag::new("Internet Radio URL", &**v),
        StandardTag::UrlLabel(v) => FormattedTag::new("Label URL", &**v),
        StandardTag::UrlOfficial(v) => FormattedTag::new("Official URL", &**v),
        StandardTag::UrlPayment(v) => FormattedTag::new("Payment URL", &**v),
        StandardTag::UrlPodcast(v) => FormattedTag::new("Podcast URL", &**v),
        StandardTag::UrlPurchase(v) => FormattedTag::new("Purchase URL", &**v),
        StandardTag::UrlSource(v) => FormattedTag::new("Source URL", &**v),
        StandardTag::Version(v) => FormattedTag::new("Version", &**v),
        StandardTag::Work(v) => FormattedTag::new("Work", &**v),
        StandardTag::Writer(v) => FormattedTag::new("Writer", &**v),
        // Missing standard tag pretty-printer.
        _ => FormattedTag::new(&tag.raw.key, format!("{}", tag.raw.value)),
    }
}
