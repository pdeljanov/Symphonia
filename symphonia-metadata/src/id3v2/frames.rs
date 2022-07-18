// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use std::borrow::Cow;
use std::io;
use std::str;

use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::{BufReader, FiniteStream, ReadBytes};
use symphonia_core::meta::{StandardTagKey, Tag, Value, Visual};

use encoding_rs::UTF_16BE;
use log::warn;

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
//   S   v2.2   v2.3    v2.4    Std. Key            Description
//   -   ----   ----    ----    ----------------    ------------------------------------------------
//       CRA    AENC                                Audio encryption
//       CRM                                        Encrypted meta frame
//   x   PIC    APIC                                Attached picture
//                      ASPI                        Audio seek point index
//   x   COM    COMM             Comment            Comments
//              COMR                                Commercial frame
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
//   x   TT1    TIT1             ContentGroup       Content group description
//   x   TT2    TIT2             TrackTitle         Title/songname/content description
//   x   TT3    TIT3             TrackSubtitle      Subtitle/Description refinement
//   x   TKE    TKEY                                Initial key
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
//   x          GRP1                                (Apple iTunes) Grouping
//   x          MVNM             MovementName       (Apple iTunes) Movement name
//   x          MVIN             MovementNumber     (Apple iTunes) Movement number
//       PCS    PCST                                (Apple iTunes) Podcast flag
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

/// The result of parsing a frame.
pub enum FrameResult {
    /// Padding was encountered instead of a frame. The remainder of the ID3v2 Tag may be skipped.
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
}

/// Makes a frame result for a frame containing invalid data.
fn invalid_data(id: &[u8]) -> Result<FrameResult> {
    Ok(FrameResult::InvalidData(as_ascii_str(id).to_string()))
}

/// Makes a frame result for an unsupported frame.
fn unsupported_frame(id: &[u8]) -> Result<FrameResult> {
    Ok(FrameResult::UnsupportedFrame(as_ascii_str(id).to_string()))
}

type FrameParser = fn(&mut BufReader<'_>, Option<StandardTagKey>, &str) -> Result<FrameResult>;

static LEGACY_FRAME_MAP: phf::Map<&[u8], &[u8; 4]> = phf::phf_map! {
    b"BUF" => b"RBUF",
    b"CNT" => b"PCNT",
    b"COM" => b"COMM",
    b"CRA" => b"AENC",
    b"EQU" => b"EQUA",
    b"ETC" => b"ETCO",
    b"GEO" => b"GEOB",
    b"IPL" => b"IPLS",
    b"LNK" => b"LINK",
    b"MCI" => b"MCDI",
    b"MLL" => b"MLLT",
    b"PCS" => b"PCST",
    b"PIC" => b"APIC",
    b"POP" => b"POPM",
    b"REV" => b"RVRB",
    b"RVA" => b"RVAD",
    b"SLT" => b"SYLT",
    b"STC" => b"SYTC",
    b"TAL" => b"TALB",
    b"TBP" => b"TBPM",
    b"TCM" => b"TCOM",
    b"TCO" => b"TCON",
    b"TCR" => b"TCOP",
    b"TDA" => b"TDAT",
    b"TDY" => b"TDLY",
    b"TEN" => b"TENC",
    b"TFT" => b"TFLT",
    b"TIM" => b"TIME",
    b"TKE" => b"TKEY",
    b"TLA" => b"TLAN",
    b"TLE" => b"TLEN",
    b"TMT" => b"TMED",
    b"TOA" => b"TOPE",
    b"TOF" => b"TOFN",
    b"TOL" => b"TOLY",
    b"TOR" => b"TORY",
    b"TOT" => b"TOAL",
    b"TP1" => b"TPE1",
    b"TP2" => b"TPE2",
    b"TP3" => b"TPE3",
    b"TP4" => b"TPE4",
    b"TPA" => b"TPOS",
    b"TPB" => b"TPUB",
    b"TRC" => b"TSRC",
    b"TRD" => b"TRDA",
    b"TRK" => b"TRCK",
    b"TS2" => b"TSO2",
    b"TSA" => b"TSOA",
    b"TSC" => b"TSOC",
    b"TSI" => b"TSIZ",
    b"TSP" => b"TSOP",
    b"TSS" => b"TSSE",
    b"TST" => b"TSOT",
    b"TT1" => b"TIT1",
    b"TT2" => b"TIT2",
    b"TT3" => b"TIT3",
    b"TXT" => b"TEXT",
    b"TXX" => b"TXXX",
    b"TYE" => b"TYER",
    b"UFI" => b"UFID",
    b"ULT" => b"USLT",
    b"WAF" => b"WOAF",
    b"WAR" => b"WOAR",
    b"WAS" => b"WOAS",
    b"WCM" => b"WCOM",
    b"WCP" => b"WCOP",
    b"WPB" => b"WPUB",
    b"WXX" => b"WXXX"
};

static FRAME_PARSERS: phf::Map<&'static [u8], (FrameParser, Option<StandardTagKey>)> = phf::phf_map! {
    // b"AENC" => read_null_frame,
    b"APIC" => (read_apic_frame as FrameParser, None),
    // b"ASPI" => read_null_frame,
    b"COMM" => (read_comm_uslt_frame, Some(StandardTagKey::Comment)),
    // b"COMR" => read_null_frame,
    // b"ENCR" => read_null_frame,
    // b"EQU2" => read_null_frame,
    // b"EQUA" => read_null_frame,
    // b"ETCO" => read_null_frame,
    // b"GEOB" => read_null_frame,
    // b"GRID" => read_null_frame,
    b"IPLS" => (read_text_frame, None),
    // b"LINK" => read_null_frame,
    b"MCDI" => (read_mcdi_frame, None),
    // b"MLLT" => read_null_frame,
    // b"OWNE" => read_null_frame,
    b"PCNT" => (read_pcnt_frame, None),
    b"POPM" => (read_popm_frame, Some(StandardTagKey::Rating)),
    // b"POSS" => read_null_frame,
    b"PRIV" => (read_priv_frame, None),
    // b"RBUF" => read_null_frame,
    // b"RVA2" => read_null_frame,
    // b"RVAD" => read_null_frame,
    // b"RVRB" => read_null_frame,
    // b"SEEK" => read_null_frame,
    // b"SIGN" => read_null_frame,
    // b"SYLT" => read_null_frame,
    // b"SYTC" => read_null_frame,
    b"TALB" => (read_text_frame, Some(StandardTagKey::Album)),
    b"TBPM" => (read_text_frame, Some(StandardTagKey::Bpm)),
    b"TCOM" => (read_text_frame, Some(StandardTagKey::Composer)),
    b"TCON" => (read_text_frame, Some(StandardTagKey::Genre)),
    b"TCOP" => (read_text_frame, Some(StandardTagKey::Copyright)),
    b"TDAT" => (read_text_frame, Some(StandardTagKey::Date)),
    b"TDEN" => (read_text_frame, Some(StandardTagKey::EncodingDate,)),
    b"TDLY" => (read_text_frame, None),
    b"TDOR" => (read_text_frame, Some(StandardTagKey::OriginalDate,)),
    b"TDRC" => (read_text_frame, Some(StandardTagKey::Date)),
    b"TDRL" => (read_text_frame, Some(StandardTagKey::ReleaseDate,)),
    b"TDTG" => (read_text_frame, Some(StandardTagKey::TaggingDate)),
    b"TENC" => (read_text_frame, Some(StandardTagKey::EncodedBy)),
    // Also Writer?
    b"TEXT" => (read_text_frame, Some(StandardTagKey::Writer)),
    b"TFLT" => (read_text_frame, None),
    b"TIME" => (read_text_frame, Some(StandardTagKey::Date)),
    b"TIPL" => (read_text_frame, None),
    b"TIT1" => (read_text_frame, Some(StandardTagKey::ContentGroup)),
    b"TIT2" => (read_text_frame, Some(StandardTagKey::TrackTitle)),
    b"TIT3" => (read_text_frame, Some(StandardTagKey::TrackSubtitle)),
    b"TKEY" => (read_text_frame, None),
    b"TLAN" => (read_text_frame, Some(StandardTagKey::Language)),
    b"TLEN" => (read_text_frame, None),
    b"TMCL" => (read_text_frame, None),
    b"TMED" => (read_text_frame, Some(StandardTagKey::MediaFormat)),
    b"TMOO" => (read_text_frame, Some(StandardTagKey::Mood)),
    b"TOAL" => (read_text_frame, Some(StandardTagKey::OriginalAlbum)),
    b"TOFN" => (read_text_frame, Some(StandardTagKey::OriginalFile)),
    b"TOLY" => (read_text_frame, Some(StandardTagKey::OriginalWriter)),
    b"TOPE" => (read_text_frame, Some(StandardTagKey::OriginalArtist)),
    b"TORY" => (read_text_frame, Some(StandardTagKey::OriginalDate)),
    b"TOWN" => (read_text_frame, None),
    b"TPE1" => (read_text_frame, Some(StandardTagKey::Artist)),
    b"TPE2" => (read_text_frame, Some(StandardTagKey::AlbumArtist)),
    b"TPE3" => (read_text_frame, Some(StandardTagKey::Conductor)),
    b"TPE4" => (read_text_frame, Some(StandardTagKey::Remixer)),
    // May be "disc number / total discs"
    b"TPOS" => (read_text_frame, Some(StandardTagKey::DiscNumber)),
    b"TPRO" => (read_text_frame, None),
    b"TPUB" => (read_text_frame, Some(StandardTagKey::Label)),
    // May be "track number / total tracks"
    b"TRCK" => (read_text_frame, Some(StandardTagKey::TrackNumber)),
    b"TRDA" => (read_text_frame, Some(StandardTagKey::Date)),
    b"TRSN" => (read_text_frame, None),
    b"TRSO" => (read_text_frame, None),
    b"TSIZ" => (read_text_frame, None),
    b"TSOA" => (read_text_frame, Some(StandardTagKey::SortAlbum)),
    b"TSOP" => (read_text_frame, Some(StandardTagKey::SortArtist)),
    b"TSOT" => (read_text_frame, Some(StandardTagKey::SortTrackTitle)),
    b"TSRC" => (read_text_frame, Some(StandardTagKey::IdentIsrc)),
    b"TSSE" => (read_text_frame, Some(StandardTagKey::Encoder)),
    b"TSST" => (read_text_frame, None),
    b"TXXX" => (read_txxx_frame, None),
    b"TYER" => (read_text_frame, Some(StandardTagKey::Date)),
    // b"UFID" => read_null_frame,
    // b"USER" => read_null_frame,
    b"USLT" => (read_comm_uslt_frame, Some(StandardTagKey::Lyrics)),
    b"WCOM" => (read_url_frame, Some(StandardTagKey::UrlPurchase)),
    b"WCOP" => (read_url_frame, Some(StandardTagKey::UrlCopyright)),
    b"WOAF" => (read_url_frame, Some(StandardTagKey::UrlOfficial)),
    b"WOAR" => (read_url_frame, Some(StandardTagKey::UrlArtist)),
    b"WOAS" => (read_url_frame, Some(StandardTagKey::UrlSource)),
    b"WORS" => (read_url_frame, Some(StandardTagKey::UrlInternetRadio)),
    b"WPAY" => (read_url_frame, Some(StandardTagKey::UrlPayment)),
    b"WPUB" => (read_url_frame, Some(StandardTagKey::UrlLabel)),
    b"WXXX" => (read_wxxx_frame, Some(StandardTagKey::Url)),
    // Apple iTunes frames
    // b"PCST" => (read_null_frame, None),
    b"GRP1" => (read_text_frame, None),
    b"MVIN" => (read_text_frame, Some(StandardTagKey::MovementNumber)),
    b"MVNM" => (read_text_frame, Some(StandardTagKey::MovementName)),
    b"TCAT" => (read_text_frame, Some(StandardTagKey::PodcastCategory)),
    b"TDES" => (read_text_frame, Some(StandardTagKey::PodcastDescription)),
    b"TGID" => (read_text_frame, Some(StandardTagKey::IdentPodcast)),
    b"TKWD" => (read_text_frame, Some(StandardTagKey::PodcastKeywords)),
    b"TSO2" => (read_text_frame, Some(StandardTagKey::SortAlbumArtist)),
    b"TSOC" => (read_text_frame, Some(StandardTagKey::SortComposer)),
    b"WFED" => (read_text_frame, Some(StandardTagKey::UrlPodcast))
};

static TXXX_FRAME_STD_KEYS: phf::Map<&'static str, StandardTagKey> = phf::phf_map! {
    "ACOUSTID FINGERPRINT" => StandardTagKey::AcoustidFingerprint,
    "ACOUSTID ID" => StandardTagKey::AcoustidId,
    "BARCODE" => StandardTagKey::IdentBarcode,
    "CATALOGNUMBER" => StandardTagKey::IdentCatalogNumber,
    "LICENSE" => StandardTagKey::License,
    "MUSICBRAINZ ALBUM ARTIST ID" => StandardTagKey::MusicBrainzAlbumArtistId,
    "MUSICBRAINZ ALBUM ID" => StandardTagKey::MusicBrainzAlbumId,
    "MUSICBRAINZ ARTIST ID" => StandardTagKey::MusicBrainzArtistId,
    "MUSICBRAINZ RELEASE GROUP ID" => StandardTagKey::MusicBrainzReleaseGroupId,
    "MUSICBRAINZ WORK ID" => StandardTagKey::MusicBrainzWorkId,
    "REPLAYGAIN_ALBUM_GAIN" => StandardTagKey::ReplayGainAlbumGain,
    "REPLAYGAIN_ALBUM_PEAK" => StandardTagKey::ReplayGainAlbumPeak,
    "REPLAYGAIN_TRACK_GAIN" => StandardTagKey::ReplayGainTrackGain,
    "REPLAYGAIN_TRACK_PEAK" => StandardTagKey::ReplayGainTrackPeak,
    "SCRIPT" => StandardTagKey::Script,
};

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
fn find_parser(id: [u8; 4]) -> Option<&'static (FrameParser, Option<StandardTagKey>)> {
    FRAME_PARSERS.get(&id)
}

/// Finds a frame parser for a "legacy" ID3v2.2 tag by finding an equivalent "modern" ID3v2.3+ frame
/// parser.
fn find_parser_legacy(id: [u8; 3]) -> Option<&'static (FrameParser, Option<StandardTagKey>)> {
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
    let (parser, std_key) = match find_parser_legacy(id) {
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

    parser(&mut BufReader::new(&data), *std_key, as_ascii_str(&id))
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
    let (parser, std_key) = match find_parser(id) {
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

    parser(&mut BufReader::new(&data), *std_key, as_ascii_str(&id))
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
    let (parser, std_key) = match find_parser(id) {
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

    // The frame body is unsynchronised. Decode the unsynchronised data back to it's original form
    // in-place before wrapping the decoded data in a BufStream for the frame parsers.
    if flags & 0x2 != 0x0 {
        let unsync_data = decode_unsynchronisation(&mut raw_data);

        parser(&mut BufReader::new(unsync_data), *std_key, as_ascii_str(&id))
    }
    // The frame body has not been unsynchronised. Wrap the raw data buffer in BufStream without any
    // additional decoding.
    else {
        parser(&mut BufReader::new(&raw_data), *std_key, as_ascii_str(&id))
    }
}

/// Reads all text frames frame except for `TXXX`.
fn read_text_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    id: &str,
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
    loop {
        let len = reader.bytes_available() as usize;

        if len > 0 {
            // Scan for text, and create a Tag.
            let text = scan_text(reader, encoding, len)?;

            tags.push(Tag::new(std_key, id, Value::from(text)));
        }
        else {
            break;
        }
    }

    Ok(FrameResult::MultipleTags(tags))
}

/// Reads a `TXXX` (user defined) text frame.
fn read_txxx_frame(
    reader: &mut BufReader<'_>,
    _: Option<StandardTagKey>,
    _: &str,
) -> Result<FrameResult> {
    // The first byte of the frame is the encoding.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid TXXX text encoding"),
    };

    // Read the description string.
    let desc = scan_text(reader, encoding, reader.bytes_available() as usize)?;

    // Some TXXX frames may be mapped to standard keys. Check if a standard key exists for the
    // description.
    let std_key = TXXX_FRAME_STD_KEYS.get(desc.as_ref()).copied();

    // Generate a key name using the description.
    let key = format!("TXXX:{}", desc);

    // Since a TXXX frame can have a null-terminated list of values, and Symphonia allows multiple
    // tags with the same key, create one Tag per listed value.
    let mut tags = Vec::<Tag>::new();

    // The remainder of the frame is one or more null-terminated strings.
    loop {
        let len = reader.bytes_available() as usize;

        if len > 0 {
            let text = scan_text(reader, encoding, len)?;
            tags.push(Tag::new(std_key, &key, Value::from(text)));
        }
        else {
            break;
        }
    }

    Ok(FrameResult::MultipleTags(tags))
}

/// Reads all URL frames except for `WXXX`.
fn read_url_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    id: &str,
) -> Result<FrameResult> {
    // Scan for a ISO-8859-1 URL string.
    let url = scan_text(reader, Encoding::Iso8859_1, reader.bytes_available() as usize)?;
    // Create a Tag.
    let tag = Tag::new(std_key, id, Value::from(url));

    Ok(FrameResult::Tag(tag))
}

/// Reads a `WXXX` (user defined) URL frame.
fn read_wxxx_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    _: &str,
) -> Result<FrameResult> {
    // The first byte of the WXXX frame is the encoding of the description.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid WXXX URL description encoding"),
    };

    // Scan for the the description string.
    let desc = format!("WXXX:{}", &scan_text(reader, encoding, reader.bytes_available() as usize)?);
    // Scan for a ISO-8859-1 URL string.
    let url = scan_text(reader, Encoding::Iso8859_1, reader.bytes_available() as usize)?;
    // Create a Tag.
    let tag = Tag::new(std_key, &desc, Value::from(url));

    Ok(FrameResult::Tag(tag))
}

/// Reads a `PRIV` (private) frame.
fn read_priv_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    _: &str,
) -> Result<FrameResult> {
    // Scan for a ISO-8859-1 owner identifier.
    let owner = format!(
        "PRIV:{}",
        &scan_text(reader, Encoding::Iso8859_1, reader.bytes_available() as usize)?
    );

    // The remainder of the frame is binary data.
    let data_buf = reader.read_buf_bytes_ref(reader.bytes_available() as usize)?;

    // Create a Tag.
    let tag = Tag::new(std_key, &owner, Value::from(data_buf));

    Ok(FrameResult::Tag(tag))
}

/// Reads a `COMM` (comment) or `USLT` (unsynchronized comment) frame.
fn read_comm_uslt_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    id: &str,
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
        format!("{}!{}", id, as_ascii_str(&lang))
    }
    else {
        id.to_string()
    };

    // Short text (content description) is next, but since there is no way to represent this in
    // Symphonia, skip it.
    scan_text(reader, encoding, reader.bytes_available() as usize)?;

    // Full text (lyrics) is last.
    let text = scan_text(reader, encoding, reader.bytes_available() as usize)?;

    // Create the tag.
    let tag = Tag::new(std_key, &key, Value::from(text));

    Ok(FrameResult::Tag(tag))
}

/// Reads a `PCNT` (total file play count) frame.
fn read_pcnt_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    id: &str,
) -> Result<FrameResult> {
    let len = reader.byte_len() as usize;

    // The play counter must be a minimum of 4 bytes long.
    if len < 4 {
        return decode_error("id3v2: play counters must be a minimum of 32bits");
    }

    // However it may be extended by an arbitrary amount of bytes (or so it would seem).
    // Practically, a 4-byte (32-bit) count is way more than enough, but we'll support up-to an
    // 8-byte (64bit) count.
    if len > 8 {
        return unsupported_error("id3v2: play counters greater than 64bits are not supported");
    }

    // The play counter is stored as an N-byte big-endian integer. Read N bytes into an 8-byte
    // buffer, making sure the missing bytes are zeroed, and then reinterpret as a 64-bit integer.
    let mut buf = [0u8; 8];
    reader.read_buf_exact(&mut buf[8 - len..])?;

    let play_count = u64::from_be_bytes(buf);

    // Create the tag.
    let tag = Tag::new(std_key, id, Value::from(play_count));

    Ok(FrameResult::Tag(tag))
}

/// Reads a `POPM` (popularimeter) frame.
fn read_popm_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    id: &str,
) -> Result<FrameResult> {
    let email = scan_text(reader, Encoding::Iso8859_1, reader.bytes_available() as usize)?;
    let key = format!("{}:{}", id, &email);

    let rating = reader.read_u8()?;

    // There's a personalized play counter here, but there is no analogue in Symphonia so don't do
    // anything with it.

    // Create the tag.
    let tag = Tag::new(std_key, &key, Value::from(rating));

    Ok(FrameResult::Tag(tag))
}

/// Reads a `MCDI` (music CD identifier) frame.
fn read_mcdi_frame(
    reader: &mut BufReader<'_>,
    std_key: Option<StandardTagKey>,
    id: &str,
) -> Result<FrameResult> {
    // The entire frame is a binary dump of a CD-DA TOC.
    let buf = reader.read_buf_bytes_ref(reader.byte_len() as usize)?;

    // Create the tag.
    let tag = Tag::new(std_key, id, Value::from(buf));

    Ok(FrameResult::Tag(tag))
}

fn read_apic_frame(
    reader: &mut BufReader<'_>,
    _: Option<StandardTagKey>,
    _: &str,
) -> Result<FrameResult> {
    // The first byte of the frame is the encoding of the text description.
    let encoding = match Encoding::parse(reader.read_byte()?) {
        Some(encoding) => encoding,
        _ => return decode_error("id3v2: invalid text encoding"),
    };

    // ASCII media (MIME) type.
    let media_type =
        scan_text(reader, Encoding::Iso8859_1, reader.bytes_available() as usize)?.into_owned();

    // Image usage.
    let usage = util::apic_picture_type_to_visual_key(u32::from(reader.read_u8()?));

    // Textual image description.
    let desc = scan_text(reader, encoding, reader.bytes_available() as usize)?;

    let tags = vec![Tag::new(Some(StandardTagKey::Description), "", Value::from(desc))];

    // The remainder of the APIC frame is the image data.
    // TODO: Apply a limit.
    let data = Box::from(reader.read_buf_bytes_available_ref());

    let visual = Visual {
        media_type,
        dimensions: None,
        bits_per_pixel: None,
        color_mode: None,
        usage,
        tags,
        data,
    };

    Ok(FrameResult::Visual(visual))
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

/// Scans up-to `scan_len` bytes from the provided `BufStream` for a string that is terminated with
/// the appropriate null terminator for the given encoding as per the ID3v2 specification. A
/// copy-on-write reference to the string excluding the null terminator is returned or an error. If
/// the scanned string is valid UTF-8, or is equivalent to UTF-8, then no copies will occur. If a
/// null terminator is not found, and `scan_len` is reached, or the stream is exhausted, all the
/// scanned bytes up-to that point are interpreted as the string.
fn scan_text<'a>(
    reader: &'a mut BufReader<'_>,
    encoding: Encoding,
    scan_len: usize,
) -> io::Result<Cow<'a, str>> {
    let buf = match encoding {
        Encoding::Iso8859_1 | Encoding::Utf8 => reader.scan_bytes_aligned_ref(&[0x00], 1, scan_len),
        Encoding::Utf16Bom | Encoding::Utf16Be => {
            reader.scan_bytes_aligned_ref(&[0x00, 0x00], 2, scan_len)
        }
    }?;

    Ok(decode_text(encoding, buf))
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
