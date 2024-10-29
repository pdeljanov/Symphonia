// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::str;
use std::sync::Arc;

use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::{BufReader, FiniteStream, ReadBytes};
use symphonia_core::meta::RawTag;
use symphonia_core::meta::RawTagSubField;
use symphonia_core::meta::{Chapter, RawValue, StandardTag, Tag, Visual};

use encoding_rs::UTF_16BE;
use lazy_static::lazy_static;
use log::warn;
use symphonia_core::units::Time;

use crate::utils::images::try_get_image_info;
use crate::utils::std_tag::*;

use super::unsync::{decode_unsynchronisation, read_syncsafe_leq32};
use super::util;

// The following is a list of all standardized ID3v2.x frames for all ID3v2 major versions and their
// implementation status ("S" column) in Symphonia.
//
// ID3v2.2 uses 3 character frame identifiers as opposed to the 4 character identifiers used in
// subsequent versions. This table may be used to map equivalent frames between the two versions.
//
// All ID3v2.3 frames are officially part of ID3v2.4 with the exception of those marked "n/a".
// However, it is likely that ID3v2.3-only frames appear in some real-world ID3v2.4 tags.
//
//   -   ----   ----    ----    ----------------    ------------------------------------------------
//   S   v2.2   v2.3    v2.4    StandardTag         Description
//   -   ----   ----    ----    ----------------    ------------------------------------------------
//       CRA    AENC                                Audio encryption
//       CRM                                        Encrypted meta frame
//   x   PIC    APIC                                Attached picture
//                      ASPI                        Audio seek point index
//   x          CHAP                                Chapter
//   x   COM    COMM             Comment            Comments
//              COMR                                Commercial frame
//   x          CTOC                                Table of contents
//              ENCR                                Encryption method registration
//       EQU    EQUA                                Equalisation
//                      EQU2                        Equalisation (2)
//       ETC    ETCO                                Event timing codes
//       GEO    GEOB                                General encapsulated object
//              GRID                                Group identification registration
//   x   IPL    IPLS    TIPL                        Involved people list
//       LNK    LINK                                Linked information
//   x   MCI    MCDI                                Music CD identifier
//       MLL    MLLT                                MPEG location lookup table
//              OWNE                                Ownership frame
//   x          PRIV                                Private frame
//   x   CNT    PCNT                                Play counter
//   x   POP    POPM             Rating             Popularimeter
//              POSS                                Position synchronisation frame
//       BUF    RBUF                                Recommended buffer size
//       RVA    RVAD                                Relative volume adjustment
//                      RVA2                        Relative volume adjustment (2)
//       REV    RVRB                                Reverb
//                      SEEK                        Seek frame
//                      SIGN                        Signature frame
//       SLT    SYLT                                Synchronized lyric/text
//       STC    SYTC                                Synchronized tempo codes
//   x   TAL    TALB             Album              Album/Movie/Show title
//   x   TBP    TBPM             Bpm                BPM (beats per minute)
//   x   TCM    TCOM             Composer           Composer
//   x   TCO    TCON             Genre              Content type
//   x   TCR    TCOP             Copyright          Copyright message
//   x   TDA    TDAT             Date               Date
//   x                  TDEN     EncodingDate       Encoding time
//   x   TDY    TDLY                                Playlist delay
//   x                  TDOR     OriginalDate       Original release time
//   x                  TDRC     Date               Recording time
//   x                  TDRL     ReleaseDate        Release time
//   x                  TDTG     TaggingDate        Tagging time
//   x   TEN    TENC             EncodedBy          Encoded by
//   x   TXT    TEXT             Writer             Lyricist/Text writer
//   x   TFT    TFLT                                File type
//   x   TIM    TIME     n/a     Date               Time
//   x   TT1    TIT1             Grouping           Content group description
//   x   TT2    TIT2             TrackTitle         Title/songname/content description
//   x   TT3    TIT3             TrackSubtitle      Subtitle/Description refinement
//   x   TKE    TKEY             InitialKey         Initial key
//   x   TLA    TLAN             Language           Language(s)
//   x   TLE    TLEN                                Length
//   x                  TMCL                        Musician credits list
//   x   TMT    TMED             MediaFormat        Media type
//   x                  TMOO     Mood               Mood
//   x   TOT    TOAL             OriginalAlbum      Original album/movie/show title
//   x   TOF    TOFN             OriginalFile       Original filename
//   x   TOL    TOLY             OriginalWriter     Original lyricist(s)/text writer(s)
//   x   TOA    TOPE             OriginalArtist     Original artist(s)/performer(s)
//   x   TOR    TORY    n/a      OriginalDate       Original release year
//   x          TOWN                                File owner/licensee
//   x   TP1    TPE1             Artist             Lead performer(s)/Soloist(s)
//   x   TP2    TPE2             AlbumArtist        Band/orchestra/accompaniment
//   x   TP3    TPE3             Performer          Conductor/performer refinement
//   x   TP4    TPE4             Remixer            Interpreted, remixed, or otherwise modified by
//   x   TPA    TPOS             TrackNumber        Part of a set
//   x                  TPRO                        Produced notice
//   x   TPB    TPUB             Label              Publisher
//   x   TRK    TRCK             TrackNumber        Track number/Position in set
//   x   TRD    TRDA    n/a      Date               Recording dates
//   x          TRSN                                Internet radio station name
//   x          TRSO                                Internet radio station owner
//   x                  TSOA     SortAlbum          Album sort order
//   x                  TSOP     SortArtist         Performer sort order
//   x                  TSOT     SortTrackTitle     Title sort order
//   x   TSI    TSIZ    n/a                         Size
//   x   TRC    TSRC             IdentIsrc          ISRC (international standard recording code)
//   x   TSS    TSSE             Encoder            Software/Hardware and settings used for encoding
//   x                  TSST                        Set subtitle
//   x   TYE    TYER    n/a      Date               Year
//   x   TXX    TXXX                                User defined text information frame
//       UFI    UFID                                Unique file identifier
//              USER                                Terms of use
//   x   ULT    USLT             Lyrics             Unsychronized lyric/text transcription
//   x   WCM    WCOM             UrlPurchase        Commercial information
//   x   WCP    WCOP             UrlCopyright       Copyright/Legal information
//   x   WAF    WOAF             UrlOfficial        Official audio file webpage
//   x   WAR    WOAR             UrlArtist          Official artist/performer webpage
//   x   WAS    WOAS             UrlSource          Official audio source webpage
//   x          WORS             UrlInternetRadio   Official internet radio station homepage
//   x          WPAY             UrlPayment         Payment
//   x   WPB    WPUB             UrlLabel           Publishers official webpage
//   x   WXX    WXXX             Url                User defined URL link frame
//   x          GRP1             Grouping           (Apple iTunes) Grouping
//   x          MVNM             MovementName       (Apple iTunes) Movement name
//   x          MVIN             MovementNumber     (Apple iTunes) Movement number
//       PCS    PCST             Podcast            (Apple iTunes) Podcast flag
//   x          TCAT             PodcastCategory    (Apple iTunes) Podcast category
//   x          TDES             PodcastDescription (Apple iTunes) Podcast description
//   x          TGID             IdentPodcast       (Apple iTunes) Podcast identifier
//   x          TKWD             PodcastKeywords    (Apple iTunes) Podcast keywords
//   x          WFED             UrlPodcast         (Apple iTunes) Podcast url
//   x   TST                     SortTrackTitle     (Apple iTunes) Title sort order
//   x   TSP                     SortArtist         (Apple iTunes) Artist order order
//   x   TSA                     SortAlbum          (Apple iTunes) Album sort order
//   x   TS2    TSO2             SortAlbumArtist    (Apple iTunes) Album artist sort order
//   x   TSC    TSOC             SortComposer       (Apple iTunes) Composer sort order
//
// Information on these frames can be found at:
//
//     ID3v2.2: http://id3.org/id3v2-00
//     ID3v2.3: http://id3.org/d3v2.3.0
//     ID3v2.4: http://id3.org/id3v2.4.0-frames

/// An ID3v2 chapter.
#[derive(Clone, Debug)]
pub struct Id3v2Chapter {
    // The chapter identifier.
    pub id: String,
    /// A counter indicating the order the chapter frame was read.
    pub read_order: usize,
    /// The chapter contents.
    pub chapter: Chapter,
}

/// An ID3v2 table of contents describes different sections and chapters of an audio stream.
#[derive(Clone, Debug, Default)]
pub struct Id3v2TableOfContents {
    /// The table of contents identifier.
    pub id: String,
    /// Indicates if this is the top-level table of contents frame. Only one table of contents
    /// frame should be marked top-level, and not be a child of any other frame.
    pub top_level: bool,
    /// Indicates if the entries should be played as a continuous ordered sequence or played
    /// individually.
    ///
    /// TODO: It is not clear if this is useful.
    #[allow(dead_code)]
    pub ordered: bool,
    /// The identifiers of the items that belong to this table of contents. These may identify
    /// a chapter or another table of contents.
    pub items: Vec<String>,
    /// The tags associated with this table of contents.
    pub tags: Vec<Tag>,
    /// The visuals associated with this table of contents.
    pub visuals: Vec<Visual>,
}

/// The result of parsing a frame.
pub enum FrameResult {
    /// Padding was encountered instead of a frame. The remainder of the ID3v2 tag may be skipped.
    Padding,
    /// An unknown frame was found and its body skipped.
    UnsupportedFrame(String),
    /// The frame was invalid and its body skipped.
    InvalidData(String),
    /// A frame was parsed and yielded a single `Tag`.
    Tag(Tag),
    /// A frame was parsed and yielded a single `Visual`.
    Visual(Visual),
    /// A frame was parsed and yielded many `Tag`s.
    MultipleTags(Vec<Tag>),
    /// A frame was parsed and yielded a chapter.
    Chapter(Id3v2Chapter),
    /// A frame was parsed and yielded a table of contents.
    TableOfContents(Id3v2TableOfContents),
}

/// Gets the minimum frame size for a major version of an ID3v2.
pub fn min_frame_size(major_version: u8) -> u64 {
    match major_version {
        2 => 6,
        3 | 4 => 10,
        _ => unreachable!("id2v3: unexpected version"),
    }
}

/// Makes a frame result for a frame containing invalid data.
fn invalid_data(id: &[u8]) -> Result<FrameResult> {
    Ok(FrameResult::InvalidData(as_ascii_str(id).to_string()))
}

/// Makes a frame result for an unsupported frame.
fn unsupported_frame(id: &[u8]) -> Result<FrameResult> {
    Ok(FrameResult::UnsupportedFrame(as_ascii_str(id).to_string()))
}

/// Useful information about a frame for a frame parser.
struct FrameInfo<'a> {
    /// The original name of the frame as written in the frame.
    id: &'a str,
    /// The major version of the ID3v2 tag containing the frame.
    major_version: u8,
}

lazy_static! {
    static ref LEGACY_FRAME_MAP: HashMap<&'static [u8; 3], &'static [u8; 4]> = {
        let mut m = HashMap::new();
        m.insert(b"BUF", b"RBUF");
        m.insert(b"CNT", b"PCNT");
        m.insert(b"COM", b"COMM");
        m.insert(b"CRA", b"AENC");
        m.insert(b"EQU", b"EQUA");
        m.insert(b"ETC", b"ETCO");
        m.insert(b"GEO", b"GEOB");
        m.insert(b"IPL", b"IPLS");
        m.insert(b"LNK", b"LINK");
        m.insert(b"MCI", b"MCDI");
        m.insert(b"MLL", b"MLLT");
        m.insert(b"PCS", b"PCST");
        m.insert(b"PIC", b"APIC");
        m.insert(b"POP", b"POPM");
        m.insert(b"REV", b"RVRB");
        m.insert(b"RVA", b"RVAD");
        m.insert(b"SLT", b"SYLT");
        m.insert(b"STC", b"SYTC");
        m.insert(b"TAL", b"TALB");
        m.insert(b"TBP", b"TBPM");
        m.insert(b"TCM", b"TCOM");
        m.insert(b"TCO", b"TCON");
        m.insert(b"TCR", b"TCOP");
        m.insert(b"TDA", b"TDAT");
        m.insert(b"TDY", b"TDLY");
        m.insert(b"TEN", b"TENC");
        m.insert(b"TFT", b"TFLT");
        m.insert(b"TIM", b"TIME");
        m.insert(b"TKE", b"TKEY");
        m.insert(b"TLA", b"TLAN");
        m.insert(b"TLE", b"TLEN");
        m.insert(b"TMT", b"TMED");
        m.insert(b"TOA", b"TOPE");
        m.insert(b"TOF", b"TOFN");
        m.insert(b"TOL", b"TOLY");
        m.insert(b"TOR", b"TORY");
        m.insert(b"TOT", b"TOAL");
        m.insert(b"TP1", b"TPE1");
        m.insert(b"TP2", b"TPE2");
        m.insert(b"TP3", b"TPE3");
        m.insert(b"TP4", b"TPE4");
        m.insert(b"TPA", b"TPOS");
        m.insert(b"TPB", b"TPUB");
        m.insert(b"TRC", b"TSRC");
        m.insert(b"TRD", b"TRDA");
        m.insert(b"TRK", b"TRCK");
        m.insert(b"TS2", b"TSO2");
        m.insert(b"TSA", b"TSOA");
        m.insert(b"TSC", b"TSOC");
        m.insert(b"TSI", b"TSIZ");
        m.insert(b"TSP", b"TSOP");
        m.insert(b"TSS", b"TSSE");
        m.insert(b"TST", b"TSOT");
        m.insert(b"TT1", b"TIT1");
        m.insert(b"TT2", b"TIT2");
        m.insert(b"TT3", b"TIT3");
        m.insert(b"TXT", b"TEXT");
        m.insert(b"TXX", b"TXXX");
        m.insert(b"TYE", b"TYER");
        m.insert(b"UFI", b"UFID");
        m.insert(b"ULT", b"USLT");
        m.insert(b"WAF", b"WOAF");
        m.insert(b"WAR", b"WOAR");
        m.insert(b"WAS", b"WOAS");
        m.insert(b"WCM", b"WCOM");
        m.insert(b"WCP", b"WCOP");
        m.insert(b"WPB", b"WPUB");
        m.insert(b"WXX", b"WXXX");
        m
    };
}

/// Function pointer to an ID3v2 frame parser.
type FrameParser = fn(BufReader<'_>, &FrameInfo<'_>, Option<RawTagParser>) -> Result<FrameResult>;

/// Map of 4 character ID3v2 frame IDs to a frame parser and optional raw tag parser pair.
type FrameParserMap = HashMap<&'static [u8; 4], (FrameParser, Option<RawTagParser>)>;

lazy_static! {
    static ref FRAME_PARSERS: FrameParserMap = {
            let mut m: FrameParserMap = HashMap::new();
            // m.insert(b"AENC", read_null_frame);
            m.insert(b"APIC", (read_apic_frame as FrameParser, None));
            // m.insert(b"ASPI", read_null_frame);
            m.insert(b"CHAP", (read_chap_frame, None));
            m.insert(b"COMM", (read_comm_uslt_frame, Some(parse_comment)));
            // m.insert(b"COMR", read_null_frame);
            m.insert(b"CTOC", (read_ctoc_frame, None));
            // m.insert(b"ENCR", read_null_frame);
            // m.insert(b"EQU2", read_null_frame);
            // m.insert(b"EQUA", read_null_frame);
            // m.insert(b"ETCO", read_null_frame);
            // m.insert(b"GEOB", read_null_frame);
            // m.insert(b"GRID", read_null_frame);
            m.insert(b"IPLS", (read_text_frame, None));
            // m.insert(b"LINK", read_null_frame);
            m.insert(b"MCDI", (read_mcdi_frame, None));
            // m.insert(b"MLLT", read_null_frame);
            // m.insert(b"OWNE", read_null_frame);
            m.insert(b"PCNT", (read_pcnt_frame, None));
            m.insert(b"POPM", (read_popm_frame, Some(parse_rating)));
            // m.insert(b"POSS", read_null_frame);
            m.insert(b"PRIV", (read_priv_frame, None));
            // m.insert(b"RBUF", read_null_frame);
            // m.insert(b"RVA2", read_null_frame);
            // m.insert(b"RVAD", read_null_frame);
            // m.insert(b"RVRB", read_null_frame);
            // m.insert(b"SEEK", read_null_frame);
            // m.insert(b"SIGN", read_null_frame);
            // m.insert(b"SYLT", read_null_frame);
            // m.insert(b"SYTC", read_null_frame);
            m.insert(b"TALB", (read_text_frame, Some(parse_album)));
            m.insert(b"TBPM", (read_text_frame, Some(parse_bpm)));
            m.insert(b"TCOM", (read_text_frame, Some(parse_composer)));
            m.insert(b"TCON", (read_text_frame, Some(parse_id3v2_genre)));
            m.insert(b"TCOP", (read_text_frame, Some(parse_copyright)));
            m.insert(b"TDAT", (read_text_frame, Some(parse_date)));
            m.insert(b"TDEN", (read_text_frame, Some(parse_encoding_date)));
            m.insert(b"TDLY", (read_text_frame, None));
            m.insert(b"TDOR", (read_text_frame, Some(parse_original_date)));
            m.insert(b"TDRC", (read_text_frame, Some(parse_date)));
            m.insert(b"TDRL", (read_text_frame, Some(parse_release_date)));
            m.insert(b"TDTG", (read_text_frame, Some(parse_tagging_date)));
            m.insert(b"TENC", (read_text_frame, Some(parse_encoded_by)));
            // Also Writer?
            m.insert(b"TEXT", (read_text_frame, Some(parse_writer)));
            m.insert(b"TFLT", (read_text_frame, None));
            m.insert(b"TIME", (read_text_frame, Some(parse_date)));
            m.insert(b"TIPL", (read_text_frame, None));
            m.insert(b"TIT1", (read_text_frame, Some(parse_grouping)));
            m.insert(b"TIT2", (read_text_frame, Some(parse_track_title)));
            m.insert(b"TIT3", (read_text_frame, Some(parse_track_subtitle)));
            m.insert(b"TKEY", (read_text_frame, Some(parse_initial_key)));
            m.insert(b"TLAN", (read_text_frame, Some(parse_language)));
            m.insert(b"TLEN", (read_text_frame, None));
            m.insert(b"TMCL", (read_text_frame, None));
            m.insert(b"TMED", (read_text_frame, Some(parse_media_format)));
            m.insert(b"TMOO", (read_text_frame, Some(parse_mood)));
            m.insert(b"TOAL", (read_text_frame, Some(parse_original_album)));
            m.insert(b"TOFN", (read_text_frame, Some(parse_original_file)));
            m.insert(b"TOLY", (read_text_frame, Some(parse_original_writer)));
            m.insert(b"TOPE", (read_text_frame, Some(parse_original_artist)));
            m.insert(b"TORY", (read_text_frame, Some(parse_original_date)));
            m.insert(b"TOWN", (read_text_frame, None));
            m.insert(b"TPE1", (read_text_frame, Some(parse_artist)));
            m.insert(b"TPE2", (read_text_frame, Some(parse_album_artist)));
            m.insert(b"TPE3", (read_text_frame, Some(parse_conductor)));
            m.insert(b"TPE4", (read_text_frame, Some(parse_remixer)));
            // May be "disc number / total discs"
            m.insert(b"TPOS", (read_text_frame, Some(parse_disc_number)));
            m.insert(b"TPRO", (read_text_frame, None));
            m.insert(b"TPUB", (read_text_frame, Some(parse_label)));
            // May be "track number / total tracks"
            m.insert(b"TRCK", (read_text_frame, Some(parse_track_number)));
            m.insert(b"TRDA", (read_text_frame, Some(parse_date)));
            m.insert(b"TRSN", (read_text_frame, Some(parse_internet_radio_name)));
            m.insert(b"TRSO", (read_text_frame, Some(parse_internet_radio_owner)));
            m.insert(b"TSIZ", (read_text_frame, None));
            m.insert(b"TSOA", (read_text_frame, Some(parse_sort_album)));
            m.insert(b"TSOP", (read_text_frame, Some(parse_sort_artist)));
            m.insert(b"TSOT", (read_text_frame, Some(parse_sort_track_title)));
            m.insert(b"TSRC", (read_text_frame, Some(parse_ident_isrc)));
            m.insert(b"TSSE", (read_text_frame, Some(parse_encoder)));
            m.insert(b"TSST", (read_text_frame, None));
            m.insert(b"TXXX", (read_txxx_frame, None));
            m.insert(b"TYER", (read_text_frame, Some(parse_date)));
            // m.insert(b"UFID", read_null_frame);
            // m.insert(b"USER", read_null_frame);
            m.insert(b"USLT", (read_comm_uslt_frame, Some(parse_lyrics)));
            m.insert(b"WCOM", (read_url_frame, Some(parse_url_purchase)));
            m.insert(b"WCOP", (read_url_frame, Some(parse_url_copyright)));
            m.insert(b"WOAF", (read_url_frame, Some(parse_url_official)));
            m.insert(b"WOAR", (read_url_frame, Some(parse_url_artist)));
            m.insert(b"WOAS", (read_url_frame, Some(parse_url_source)));
            m.insert(b"WORS", (read_url_frame, Some(parse_url_internet_radio)));
            m.insert(b"WPAY", (read_url_frame, Some(parse_url_payment)));
            m.insert(b"WPUB", (read_url_frame, Some(parse_url_label)));
            m.insert(b"WXXX", (read_wxxx_frame, Some(parse_url)));
            // Apple iTunes frames
            // m.insert(b"PCST", (read_null_frame, None));
            m.insert(b"GRP1", (read_text_frame, Some(parse_grouping)));
            m.insert(b"MVIN", (read_text_frame, Some(parse_movement_number)));
            m.insert(b"MVNM", (read_text_frame, Some(parse_movement_name)));
            m.insert(b"TCAT", (read_text_frame, Some(parse_podcast_category)));
            m.insert(b"TDES", (read_text_frame, Some(parse_podcast_description)));
            m.insert(b"TGID", (read_text_frame, Some(parse_ident_podcast)));
            m.insert(b"TKWD", (read_text_frame, Some(parse_podcast_keywords)));
            m.insert(b"TSO2", (read_text_frame, Some(parse_sort_album_artist)));
            m.insert(b"TSOC", (read_text_frame, Some(parse_sort_composer)));
            m.insert(b"WFED", (read_text_frame, Some(parse_url_podcast)));
            m
        };
}

lazy_static! {
    static ref TXXX_FRAME_STD_KEYS: RawTagParserMap = {
        let mut m: RawTagParserMap = HashMap::new();
        // m.insert("itunesadvistory", parse_advisory);
        m.insert("acoustid fingerprint", parse_acoustid_fingerprint);
        m.insert("acoustid id", parse_acoustid_id);
        m.insert("albumartistsort", parse_sort_album_artist);
        m.insert("barcode", parse_ident_barcode);
        m.insert("catalognumber", parse_ident_catalog_number);
        m.insert("license", parse_license);
        m.insert("musicbrainz album artist id", parse_musicbrainz_album_artist_id);
        m.insert("musicbrainz album id", parse_musicbrainz_album_id);
        m.insert("musicbrainz album status", parse_musicbrainz_release_status);
        m.insert("musicbrainz album type", parse_musicbrainz_release_type);
        m.insert("musicbrainz artist id", parse_musicbrainz_artist_id);
        m.insert("musicbrainz release group id", parse_musicbrainz_release_group_id);
        m.insert("musicbrainz work id", parse_musicbrainz_work_id);
        m.insert("replaygain_album_gain", parse_replaygain_album_gain);
        m.insert("replaygain_album_peak", parse_replaygain_album_peak);
        m.insert("replaygain_track_gain", parse_replaygain_track_gain);
        m.insert("replaygain_track_peak", parse_replaygain_track_peak);
        m.insert("script", parse_script);
        m.insert("work", parse_work);
        m
    };
}

/// Validates that a frame id only contains the uppercase letters A-Z, and digits 0-9.
fn validate_frame_id(id: &[u8]) -> bool {
    // Only frame IDs with 3 or 4 characters are valid.
    if id.len() != 4 && id.len() != 3 {
        return false;
    }

    // Character:   '/'   [ '0'  ...  '9' ]  ':'  ...  '@'  [ 'A'  ...  'Z' ]   '['
    // ASCII Code:  0x2f  [ 0x30 ... 0x39 ]  0x3a ... 0x40  [ 0x41 ... 0x5a ]  0x5b
    id.iter().filter(|&b| !((*b >= b'0' && *b <= b'9') || (*b >= b'A' && *b <= b'Z'))).count() == 0
}

/// Validates that a language code conforms to the ISO-639-2 standard. That is to say, the code is
/// composed of 3 characters, each character being between lowercase letters a-z.
fn validate_lang_code(code: [u8; 3]) -> bool {
    code.iter().filter(|&c| *c < b'a' || *c > b'z').count() == 0
}

/// Gets a slice of ASCII bytes as a string slice.
///
/// Assumes the bytes are valid ASCII characters. Panics otherwise.
fn as_ascii_str(id: &[u8]) -> &str {
    std::str::from_utf8(id).unwrap()
}

/// Finds a frame parser for "modern" ID3v2.3 or ID3v2.4 tags.
fn find_parser(id: [u8; 4]) -> Option<&'static (FrameParser, Option<RawTagParser>)> {
    FRAME_PARSERS.get(&id)
}

/// Finds a frame parser for a "legacy" ID3v2.2 tag by finding an equivalent "modern" ID3v2.3+ frame
/// parser.
fn find_parser_legacy(id: [u8; 3]) -> Option<&'static (FrameParser, Option<RawTagParser>)> {
    match LEGACY_FRAME_MAP.get(&id) {
        Some(id) => find_parser(**id),
        _ => None,
    }
}

/// Read an ID3v2.2 frame.
pub fn read_id3v2p2_frame<B: ReadBytes>(reader: &mut B) -> Result<FrameResult> {
    let id = reader.read_triple_bytes()?;

    // Check if the frame id contains valid characters. If it does not, then assume the rest of the
    // tag is padding. As per the specification, padding should be all 0s, but there are some tags
    // which don't obey the specification.
    if !validate_frame_id(&id) {
        // As per the specification, padding should be all 0s, but there are some tags which don't
        // obey the specification.
        if id != [0, 0, 0] {
            warn!("padding bytes not zero");
        }

        return Ok(FrameResult::Padding);
    }

    let size = u64::from(reader.read_be_u24()?);

    // Find a parser for the frame. If there is none, skip over the remainder of the frame as it
    // cannot be parsed.
    let (parser, raw_tag_parser) = match find_parser_legacy(id) {
        Some(p) => p,
        None => {
            reader.ignore_bytes(size)?;
            return unsupported_frame(&id);
        }
    };

    // A frame must be atleast 1 byte as per the specification.
    if size == 0 {
        return invalid_data(&id);
    }

    let data = reader.read_boxed_slice_exact(size as usize)?;
    let info = FrameInfo { id: as_ascii_str(&id), major_version: 2 };

    parser(BufReader::new(&data), &info, *raw_tag_parser)
}

/// Read an ID3v2.3 frame.
pub fn read_id3v2p3_frame<B: ReadBytes>(reader: &mut B) -> Result<FrameResult> {
    let id = reader.read_quad_bytes()?;

    // Check if the frame id contains valid characters. If it does not, then assume the rest of the
    // tag is padding. As per the specification, padding should be all 0s, but there are some tags
    // which don't obey the specification.
    if !validate_frame_id(&id) {
        // As per the specification, padding should be all 0s, but there are some tags which don't
        // obey the specification.
        if id != [0, 0, 0, 0] {
            warn!("padding bytes not zero");
        }

        return Ok(FrameResult::Padding);
    }

    let mut size = u64::from(reader.read_be_u32()?);
    let flags = reader.read_be_u16()?;

    // Unused flag bits must be cleared.
    if flags & 0x1f1f != 0x0 {
        return decode_error("id3v2: unused flag bits are not cleared");
    }

    // Find a parser for the frame. If there is none, skip over the remainder of the frame as it
    // cannot be parsed.
    let (parser, raw_tag_parser) = match find_parser(id) {
        Some(p) => p,
        None => {
            reader.ignore_bytes(size)?;
            return unsupported_frame(&id);
        }
    };

    // Frame zlib DEFLATE compression usage flag.
    // TODO: Implement decompression if it is actually used in the real world.
    if flags & 0x80 != 0x0 {
        reader.ignore_bytes(size)?;
        return unsupported_error("id3v2: compressed frames are not supported");
    }

    // Frame encryption usage flag. This will likely never be supported since encryption methods are
    // vendor-specific.
    if flags & 0x4 != 0x0 {
        reader.ignore_bytes(size)?;
        return unsupported_error("id3v2: encrypted frames are not supported");
    }

    // Frame group identifier byte. Used to group a set of frames. There is no analogue in
    // Symphonia.
    if size >= 1 && (flags & 0x20) != 0x0 {
        reader.read_byte()?;
        size -= 1;
    }

    // A frame must be atleast 1 byte as per the specification.
    if size == 0 {
        return invalid_data(&id);
    }

    let data = reader.read_boxed_slice_exact(size as usize)?;
    let info = FrameInfo { id: as_ascii_str(&id), major_version: 4 };

    parser(BufReader::new(&data), &info, *raw_tag_parser)
}

/// Read an ID3v2.4 frame.
pub fn read_id3v2p4_frame<B: ReadBytes + FiniteStream>(reader: &mut B) -> Result<FrameResult> {
    let id = reader.read_quad_bytes()?;

    // Check if the frame id contains valid characters. If it does not, then assume the rest of the
    // tag is padding.
    if !validate_frame_id(&id) {
        // As per the specification, padding should be all 0s, but there are some tags which don't
        // obey the specification.
        if id != [0, 0, 0, 0] {
            warn!("padding bytes not zero");
        }

        return Ok(FrameResult::Padding);
    }

    let mut size = u64::from(read_syncsafe_leq32(reader, 28)?);
    let flags = reader.read_be_u16()?;

    // Unused flag bits must be cleared.
    if flags & 0x8fb0 != 0x0 {
        return decode_error("id3v2: unused flag bits are not cleared");
    }

    // Find a parser for the frame. If there is none, skip over the remainder of the frame as it
    // cannot be parsed.
    let (parser, raw_tag_parser) = match find_parser(id) {
        Some(p) => p,
        None => {
            reader.ignore_bytes(size)?;
            return unsupported_frame(&id);
        }
    };

    // Frame zlib DEFLATE compression usage flag.
    // TODO: Implement decompression if it is actually used in the real world.
    if flags & 0x8 != 0x0 {
        reader.ignore_bytes(size)?;
        return unsupported_error("id3v2: compressed frames are not supported");
    }

    // Frame encryption usage flag. This will likely never be supported since encryption methods are
    // vendor-specific.
    if flags & 0x4 != 0x0 {
        reader.ignore_bytes(size)?;
        return unsupported_error("id3v2: encrypted frames are not supported");
    }

    // Frame group identifier byte. Used to group a set of frames. There is no analogue in
    // Symphonia.
    if size >= 1 && (flags & 0x40) != 0x0 {
        reader.read_byte()?;
        size -= 1;
    }

    // The data length indicator is optional in the frame header. This field indicates the original
    // size of the frame body before compression, encryption, and/or unsynchronisation. It is
    // mandatory if encryption or compression are used, but only encouraged for unsynchronisation.
    // It's not that helpful, so we just ignore it.
    if size >= 4 && (flags & 0x1) != 0x0 {
        read_syncsafe_leq32(reader, 28)?;
        size -= 4;
    }

    // A frame must be atleast 1 byte as per the specification.
    if size == 0 {
        return invalid_data(&id);
    }

    // Read the frame body into a new buffer. This is, unfortunate. The original plan was to use an
    // UnsyncStream to transparently decode the unsynchronisation stream, however, the format does
    // not make this easy. For one, the decoded data length field is optional. This is fine..
    // sometimes. For example, text frames should have their text field terminated by 0x00 or
    // 0x0000, so it /should/ be possible to scan for the termination. However, despite being
    // mandatory per the specification, not all tags have terminated text fields. It gets even worse
    // when your text field is actually a list. The condition to continue scanning for terminations
    // is if there is more data left in the frame body. However, the frame body length is the
    // unsynchronised length, not the decoded length (that part is optional). If we scan for a
    // termination, we know the length of the /decoded/ data, not how much data we actually consumed
    //  to obtain that decoded data. Therefore we exceed the bounds of the frame. With this in mind,
    // the easiest thing to do is just load frame body into memory, subject to a memory limit, and
    // decode it before passing it to a parser. Therefore we always know the decoded data length and
    // the typical algorithms work. It should be noted this isn't necessarily worse. Scanning for a
    // termination still would've required a buffer to scan into with the UnsyncStream, whereas we
    // can just get references to the decoded data buffer we create here.
    //
    // You win some, you lose some. :)
    let mut raw_data = reader.read_boxed_slice_exact(size as usize)?;

    let info = FrameInfo { id: as_ascii_str(&id), major_version: 4 };

    // The frame body is unsynchronised. Decode the unsynchronised data back to it's original form
    // in-place before wrapping the decoded data in a BufStream for the frame parsers.
    if flags & 0x2 != 0x0 {
        let unsync_data = decode_unsynchronisation(&mut raw_data);

        parser(BufReader::new(unsync_data), &info, *raw_tag_parser)
    }
    // The frame body has not been unsynchronised. Wrap the raw data buffer in BufStream without any
    // additional decoding.
    else {
        parser(BufReader::new(&raw_data), &info, *raw_tag_parser)
    }
}

/// Reads all text frames frame except for `TXXX`.
fn read_text_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    raw_tag_parser: Option<RawTagParser>,
) -> Result<FrameResult> {
    // The first byte of the frame is the encoding.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid text encoding"),
    };

    // Since a text frame can have a null-terminated list of values, and Symphonia allows multiple
    // tags with the same key, create one Tag per listed value.
    let mut tags = Vec::<Tag>::new();

    // The remainder of the frame is one or more null-terminated strings.
    while reader.bytes_available() > 0 {
        let text = read_text(&mut reader, encoding)?;

        match map_raw_tag(RawTag::new(info.id, text), raw_tag_parser) {
            FrameResult::Tag(tag) => tags.push(tag),
            FrameResult::MultipleTags(multi) => tags.extend(multi.into_iter()),
            _ => (),
        }
    }

    Ok(FrameResult::MultipleTags(tags))
}

/// Reads a `TXXX` (user defined) text frame.
fn read_txxx_frame(
    mut reader: BufReader<'_>,
    _: &FrameInfo<'_>,
    _: Option<RawTagParser>,
) -> Result<FrameResult> {
    // The first byte of the frame is the encoding.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid TXXX text encoding"),
    };

    // Read the description string.
    let desc = read_text(&mut reader, encoding)?;

    // Generate a key name using the description.
    let key = format!("TXXX:{}", desc);

    // Some TXXX frames may be mapped to standard keys. Check if a standard key exists for the
    // description.
    let raw_tag_parser = TXXX_FRAME_STD_KEYS.get(desc.to_ascii_lowercase().as_str()).copied();

    // Since a TXXX frame can have a null-terminated list of values, and Symphonia allows multiple
    // tags with the same key, create one Tag per listed value.
    let mut tags = Vec::<Tag>::new();

    // The remainder of the frame is one or more null-terminated strings.
    while reader.bytes_available() > 0 {
        let text = read_text(&mut reader, encoding)?;

        match map_raw_tag(RawTag::new(key.clone(), text), raw_tag_parser) {
            FrameResult::Tag(tag) => tags.push(tag),
            FrameResult::MultipleTags(multi) => tags.extend(multi.into_iter()),
            _ => (),
        }
    }

    Ok(FrameResult::MultipleTags(tags))
}

/// Reads all URL frames except for `WXXX`.
fn read_url_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    raw_tag_parser: Option<RawTagParser>,
) -> Result<FrameResult> {
    // Scan for a ISO-8859-1 URL string.
    let url = read_text(&mut reader, Encoding::Iso8859_1)?;

    // Create the tag.
    Ok(map_raw_tag(RawTag::new(info.id, url), raw_tag_parser))
}

/// Reads a `WXXX` (user defined) URL frame.
fn read_wxxx_frame(
    mut reader: BufReader<'_>,
    _: &FrameInfo<'_>,
    raw_tag_parser: Option<RawTagParser>,
) -> Result<FrameResult> {
    // The first byte of the WXXX frame is the encoding of the description.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid WXXX URL description encoding"),
    };

    // Scan for the the description string.
    let key = format!("WXXX:{}", read_text(&mut reader, encoding)?);
    // Scan for a ISO-8859-1 URL string.
    let url = read_text(&mut reader, Encoding::Iso8859_1)?;

    // Create the tag.
    Ok(map_raw_tag(RawTag::new(key, url), raw_tag_parser))
}

/// Reads a `PRIV` (private) frame.
fn read_priv_frame(
    mut reader: BufReader<'_>,
    _: &FrameInfo<'_>,
    raw_tag_parser: Option<RawTagParser>,
) -> Result<FrameResult> {
    // Scan for a ISO-8859-1 owner identifier.
    let owner = format!("PRIV:{}", read_text(&mut reader, Encoding::Iso8859_1)?);

    // The remainder of the frame is binary data.
    let data = reader.read_buf_bytes_ref(reader.bytes_available() as usize)?;

    // Create the tag.
    Ok(map_raw_tag(RawTag::new(owner, data), raw_tag_parser))
}

/// Reads a `COMM` (comment) or `USLT` (unsynchronized comment) frame.
fn read_comm_uslt_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    raw_tag_parser: Option<RawTagParser>,
) -> Result<FrameResult> {
    // The first byte of the frame is the encoding of the description.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid text encoding"),
    };

    // The next three bytes are the language.
    let lang = reader.read_triple_bytes()?;

    // Encode the language into the key of the comment Tag. Since many files don't use valid
    // ISO-639-2 language codes, we'll just skip the language code if it doesn't validate. Returning
    // an error would break far too many files to be worth it.
    let key = if validate_lang_code(lang) {
        format!("{}!{}", info.id, as_ascii_str(&lang))
    }
    else {
        info.id.to_string()
    };

    // Short text (content description) is next, but since there is no way to represent this in
    // Symphonia, skip it.
    read_text(&mut reader, encoding)?;

    // Full text (lyrics) is last.
    let text = read_text(&mut reader, encoding)?;

    // Create the tag.
    Ok(map_raw_tag(RawTag::new(key, text), raw_tag_parser))
}

/// Reads a `PCNT` (total file play count) frame.
fn read_pcnt_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    raw_tag_parser: Option<RawTagParser>,
) -> Result<FrameResult> {
    // Read the mandatory play counter.
    let play_count = match read_play_counter(&mut reader)? {
        Some(count) => count,
        _ => return decode_error("id3v2: invalid play counter"),
    };

    // Create the tag.
    Ok(map_raw_tag(RawTag::new(info.id, play_count), raw_tag_parser))
}

/// Reads a `POPM` (popularimeter) frame.
fn read_popm_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    _: Option<RawTagParser>,
) -> Result<FrameResult> {
    let mut fields = Vec::new();

    // Read the email of the user this frame belongs to. Add it to the sub-fields of the tag.
    let email = read_text(&mut reader, Encoding::Iso8859_1)?;
    fields.push(RawTagSubField::new("EMAIL", email));

    // Read the rating.
    let rating = reader.read_u8()?;

    // Read the optional play counter. Add it to the sub-fields of the tag.
    if let Some(play_counter) = read_play_counter(&mut reader)? {
        fields.push(RawTagSubField::new("PLAY_COUNTER", play_counter));
    }

    // The primary value of this frame is the rating as it is mandatory whereas the play counter is
    // not. Add the user's email and play counter as sub-fields.
    let raw = RawTag::new_with_sub_fields(info.id, rating, fields.into_boxed_slice());

    // Create the tag.
    let tag = Tag::new(raw);

    Ok(FrameResult::Tag(tag))
}

/// Reads a `MCDI` (music CD identifier) frame.
fn read_mcdi_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    _: Option<RawTagParser>,
) -> Result<FrameResult> {
    // The entire frame is a binary dump of a CD-DA TOC.
    let buf = reader.read_buf_bytes_ref(reader.byte_len() as usize)?;

    // TODO: Parse binary MCDI into hex-string based format as specified for the StandardTag.

    // Create the tag.
    let tag = Tag::new_from_parts(info.id, buf, None);

    Ok(FrameResult::Tag(tag))
}

fn read_apic_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    _: Option<RawTagParser>,
) -> Result<FrameResult> {
    // The first byte of the frame is the encoding of the text description.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid text encoding"),
    };

    // Image format/media type
    let media_type = if info.id == "PIC" {
        // Legacy PIC frames use a 3 character identifier. Only JPG and PNG are well-defined.
        match &reader.read_triple_bytes()? {
            b"JPG" => Some("image/jpeg"),
            b"PNG" => Some("image/png"),
            b"BMP" => Some("image/bmp"),
            b"GIF" => Some("image/gif"),
            _ => None,
        }
        .map(|s| s.to_string())
    }
    else {
        // APIC frames use a null-terminated ASCII media-type string.
        read_text_not_empty(&mut reader, Encoding::Iso8859_1)?
    };

    // Image usage.
    let usage = util::apic_picture_type_to_visual_key(u32::from(reader.read_u8()?));

    let mut tags = vec![];

    // Null-teriminated image description in specified encoding.
    if let Some(desc) = read_text_not_empty(&mut reader, encoding)? {
        tags.push(Tag::new_from_parts("", "", Some(StandardTag::Description(Arc::from(desc)))));
    }

    // The remainder of the APIC frame is the image data.
    // TODO: Apply a limit.
    let data = Box::from(reader.read_buf_bytes_available_ref());

    // Try to get information about the image.
    let image_info = try_get_image_info(&data);

    let visual = Visual {
        media_type: image_info.as_ref().map(|info| info.media_type.clone()).or(media_type),
        dimensions: image_info.as_ref().map(|info| info.dimensions),
        color_mode: image_info.as_ref().map(|info| info.color_mode),
        usage,
        tags,
        data,
    };

    Ok(FrameResult::Visual(visual))
}

/// Reads a `CHAP` (chapter) frame.
fn read_chap_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    _: Option<RawTagParser>,
) -> Result<FrameResult> {
    // Read the element ID.
    let id = read_text(&mut reader, Encoding::Iso8859_1)?;

    // Start time in ms.
    let start_ms = reader.read_be_u32()?;
    // End time in ms.
    let end_ms = reader.read_be_u32()?;
    // Optional start position in bytes.
    let start_byte = match reader.read_be_u32()? {
        u32::MAX => None,
        start_byte => Some(u64::from(start_byte)),
    };
    // Optional end position in bytes.
    let end_byte = match reader.read_be_u32()? {
        u32::MAX => None,
        end_byte => Some(u64::from(end_byte)),
    };

    // Read supplemental tags.
    let mut tags = vec![];
    let mut visuals = vec![];

    while reader.bytes_available() >= min_frame_size(info.major_version) {
        let frame = match info.major_version {
            2 => read_id3v2p2_frame(&mut reader),
            3 => read_id3v2p3_frame(&mut reader),
            4 => read_id3v2p4_frame(&mut reader),
            _ => break,
        }?;

        match frame {
            FrameResult::MultipleTags(tag_list) => tags.extend(tag_list.into_iter()),
            FrameResult::Tag(tag) => tags.push(tag),
            FrameResult::Visual(visual) => visuals.push(visual),
            _ => {}
        }
    }

    let chapter = Id3v2Chapter {
        id,
        read_order: 0,
        chapter: Chapter {
            start_time: Time::from_ms(u64::from(start_ms)),
            end_time: Some(Time::from_ms(u64::from(end_ms))),
            start_byte,
            end_byte,
            tags,
            visuals,
        },
    };

    Ok(FrameResult::Chapter(chapter))
}

/// Reads a `CTOC` (table of contents) frame.
fn read_ctoc_frame(
    mut reader: BufReader<'_>,
    info: &FrameInfo<'_>,
    _: Option<RawTagParser>,
) -> Result<FrameResult> {
    // Read for the element ID.
    let id = read_text(&mut reader, Encoding::Iso8859_1)?;

    // Read the flags.
    // - Bit 0 is the "ordered" bit. Indicates if the items should be played in order, or
    //   individually.
    // - Bit 1 is the "top-level" bit. Indicates if this table of contents is the root.
    let flags = reader.read_u8()?;
    // The number of items in this table of contents
    let entry_count = reader.read_u8()?;

    // Read child item element IDs.
    let mut items = Vec::with_capacity(usize::from(entry_count));

    for _ in 0..entry_count {
        let name = read_text(&mut reader, Encoding::Iso8859_1)?;
        items.push(name);
    }

    // Read supplemental tags.
    let mut tags = Vec::new();
    let mut visuals = Vec::new();

    while reader.bytes_available() >= min_frame_size(info.major_version) {
        let frame = match info.major_version {
            2 => read_id3v2p2_frame(&mut reader),
            3 => read_id3v2p3_frame(&mut reader),
            4 => read_id3v2p4_frame(&mut reader),
            _ => break,
        }?;

        match frame {
            FrameResult::MultipleTags(tag_list) => tags.extend(tag_list.into_iter()),
            FrameResult::Tag(tag) => tags.push(tag),
            FrameResult::Visual(visual) => visuals.push(visual),
            _ => {}
        }
    }

    let toc = Id3v2TableOfContents {
        id,
        top_level: flags & 2 != 0,
        ordered: flags & 1 != 0,
        items,
        tags,
        visuals,
    };

    Ok(FrameResult::TableOfContents(toc))
}

/// Attempt to map the raw tag into one or more standard tags.
fn map_raw_tag(raw: RawTag, parser: Option<RawTagParser>) -> FrameResult {
    if let Some(parser) = parser {
        // A parser was provided.
        if let RawValue::String(value) = &raw.value {
            // Parse and return frame result.
            match parser(value.clone()) {
                [Some(std), None] => {
                    // One raw tag yielded one standard tag.
                    return FrameResult::Tag(Tag::new_std(raw, std));
                }
                [None, Some(std)] => {
                    // One raw tag yielded one standard tag.
                    return FrameResult::Tag(Tag::new_std(raw, std));
                }
                [Some(std0), Some(std1)] => {
                    // One raw tag yielded two standards tags.
                    let tags = vec![Tag::new_std(raw.clone(), std0), Tag::new_std(raw, std1)];
                    return FrameResult::MultipleTags(tags);
                }
                // The raw value could not be parsed.
                _ => (),
            }
        }
    }

    // Could not parse, add a raw tag.
    FrameResult::Tag(Tag::new(raw))
}

/// Enumeration of valid encodings for text fields in ID3v2 tags
#[derive(Copy, Clone, Debug)]
enum Encoding {
    /// ISO-8859-1 (aka Latin-1) characters in the range 0x20-0xFF.
    Iso8859_1,
    /// UTF-16 (or UCS-2) with a byte-order-mark (BOM). If the BOM is missing, big-endian encoding
    /// is assumed.
    Utf16Bom,
    /// UTF-16 big-endian without a byte-order-mark (BOM).
    Utf16Be,
    /// UTF-8.
    Utf8,
}

impl Encoding {
    fn parse(encoding: u8) -> Option<Encoding> {
        match encoding {
            // ISO-8859-1 terminated with 0x00.
            0 => Some(Encoding::Iso8859_1),
            // UTF-16 with byte order marker (BOM), terminated with 0x00 0x00.
            1 => Some(Encoding::Utf16Bom),
            // UTF-16BE without byte order marker (BOM), terminated with 0x00 0x00.
            2 => Some(Encoding::Utf16Be),
            // UTF-8 terminated with 0x00.
            3 => Some(Encoding::Utf8),
            // Invalid encoding.
            _ => None,
        }
    }
}

/// Same behaviour as `read_text`, but ignores empty strings.
fn read_text_not_empty(
    reader: &mut BufReader<'_>,
    encoding: Encoding,
) -> io::Result<Option<String>> {
    Ok(Some(read_text(reader, encoding)?).filter(|text| !text.is_empty()))
}

/// Read a null-terminated string of the specified encoding from the stream. If the stream ends
/// before the null-terminator is reached, all the bytes up-to that point are interpreted as the
/// string.
fn read_text(reader: &mut BufReader<'_>, encoding: Encoding) -> io::Result<String> {
    let max_len = reader.bytes_available() as usize;

    let buf = match encoding {
        Encoding::Iso8859_1 | Encoding::Utf8 => {
            // Byte aligned encodings. The null-terminator is 1 byte.
            reader.scan_bytes_aligned_ref(&[0x00], 1, max_len)
        }
        Encoding::Utf16Bom | Encoding::Utf16Be => {
            // Two-byte aligned encodings. The null-terminator is 2 bytes.
            reader.scan_bytes_aligned_ref(&[0x00, 0x00], 2, max_len)
        }
    }?;

    Ok(String::from(decode_text(encoding, buf)))
}

/// Decodes a slice of bytes containing encoded text into a UTF-8 `str`. Trailing null terminators
/// are removed, and any invalid characters are replaced with the [U+FFFD REPLACEMENT CHARACTER].
fn decode_text(encoding: Encoding, data: &[u8]) -> Cow<'_, str> {
    let mut end = data.len();

    match encoding {
        Encoding::Iso8859_1 => {
            // The ID3v2 specification says that only ISO-8859-1 characters between 0x20 to 0xFF,
            // inclusive, are considered valid. Any null terminator(s) (trailing 0x00 byte for
            // ISO-8859-1) will also be removed.
            //
            // TODO: Improve this conversion by returning a copy-on-write str sliced from data if
            // all characters are > 0x1F and < 0x80. Fallback to the iterator approach otherwise.
            data.iter().filter(|&b| *b > 0x1f).map(|&b| b as char).collect()
        }
        Encoding::Utf8 => {
            // Remove any null terminator(s) (trailing 0x00 byte for UTF-8).
            while end > 0 {
                if data[end - 1] != 0 {
                    break;
                }
                end -= 1;
            }
            String::from_utf8_lossy(&data[..end])
        }
        Encoding::Utf16Bom | Encoding::Utf16Be => {
            // Remove any null terminator(s) (trailing [0x00, 0x00] bytes for UTF-16 variants).
            while end > 1 {
                if data[end - 2] != 0x0 || data[end - 1] != 0x0 {
                    break;
                }
                end -= 2;
            }
            // Decode UTF-16 to UTF-8. If a byte-order-mark is present, UTF_16BE.decode() will use
            // the indicated endianness. Otherwise, big endian is assumed.
            UTF_16BE.decode(&data[..end]).0
        }
    }
}

/// Read a variably sized play counter.
fn read_play_counter(reader: &mut BufReader<'_>) -> Result<Option<u64>> {
    let len = reader.bytes_available() as usize;

    // A length of 0 indicates no play counter.
    if len == 0 {
        return Ok(None);
    }

    // A valid play counter must be a minimum of 4 bytes long.
    if len < 4 {
        return decode_error("id3v2: play counter must be a minimum of 32 bits");
    }

    // However it may be extended by an arbitrary amount of bytes (or so it would seem).
    // Practically, a 4-byte (32-bit) count is way more than enough, but we'll support up-to an
    // 8-byte (64bit) count.
    if len > 8 {
        return unsupported_error("id3v2: play counter greater-than 64 bits are not supported");
    }

    // The play counter is stored as an N-byte big-endian integer. Read N bytes into an 8-byte
    // buffer, making sure the missing bytes are zeroed, and then reinterpret as a 64-bit integer.
    let mut buf = [0u8; 8];
    reader.read_buf_exact(&mut buf[8 - len..])?;

    Ok(Some(u64::from_be_bytes(buf)))
}
