// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use sonata_core::errors::{Result, unsupported_error, decode_error};
use sonata_core::io::{Bytestream, BufStream, FiniteStream};

use std::collections::HashMap;
use lazy_static::lazy_static;
use encoding_rs::UTF_16BE;

use super::{decode_unsynchronisation, read_syncsafe_leq32};
use super::UnsyncStream;

// The following is a list of all standardized ID3v2.x frames for all ID3v2 major versions and their implementation
// status ("S" column) in Sonata.
//
// ID3v2.2 uses 3 character frame identifiers as opposed to the 4 character identifiers used in subsequent versions. 
// This table may be used to map equivalent frames between the two versions.
//
// All ID3v2.3 frames are officially part of ID3v2.4 with the exception of those marked "n/a". However, it is likely 
// that ID3v2.3-only frames appear in some real-world ID3v2.4 tags.
//
//   -   ----   ----    ----    ------------------------------------------------
//   S   v2.2   v2.3    v2.4    Description
//   -   ----   ----    ----    ------------------------------------------------
//       CRA    AENC            Audio encryption
//       CRM                    Encrypted meta frame
//       PIC    APIC            Attached picture
//                      ASPI    Audio seek point index
//       COM    COMM            Comments
//              COMR            Commercial frame
//              ENCR            Encryption method registration
//       EQU    EQUA            Equalisation
//                      EQU2    Equalisation (2)
//       ETC    ETCO            Event timing codes
//       GEO    GEOB            General encapsulated object
//              GRID            Group identification registration
//       IPL    IPLS    TIPL    Involved people list
//       LNK    LINK            Linked information
//       MCI    MCDI            Music CD identifier
//       MLL    MLLT            MPEG location lookup table
//              OWNE            Ownership frame
//              PRIV            Private frame
//       CNT    PCNT            Play counter
//       POP    POPM            Popularimeter
//              POSS            Position synchronisation frame
//       BUF    RBUF            Recommended buffer size
//       RVA    RVAD            Relative volume adjustment
//                      RVA2    Relative volume adjustment (2)
//       REV    RVRB            Reverb
//                      SEEK    Seek frame
//                      SIGN    Signature frame
//       SLT    SYLT            Synchronized lyric/text
//       STC    SYTC            Synchronized tempo codes
//       TAL    TALB            Album/Movie/Show title
//       TBP    TBPM            BPM (beats per minute)
//       TCM    TCOM            Composer
//       TCO    TCON            Content type
//       TCR    TCOP            Copyright message
//       TDA    TDAT            Date
//                      TDEN    Encoding time
//       TDY    TDLY            Playlist delay
//                      TDOR    Original release time
//                      TDRC    Recording time
//                      TDRL    Release time
//                      TDTG    Tagging time
//       TEN    TENC            Encoded by
//       TXT    TEXT            Lyricist/Text writer
//       TFT    TFLT            File type
//       TIM    TIME     n/a    Time
//       TT1    TIT1            Content group description
//       TT2    TIT2            Title/songname/content description
//       TT3    TIT3            Subtitle/Description refinement
//       TKE    TKEY            Initial key
//       TLA    TLAN            Language(s)
//       TLE    TLEN            Length
//                      TMCL    Musician credits list
//       TMT    TMED            Media type
//                      TMOO    Mood
//       TOT    TOAL            Original album/movie/show title
//       TOF    TOFN            Original filename
//       TOL    TOLY            Original lyricist(s)/text writer(s)
//       TOA    TOPE            Original artist(s)/performer(s)
//       TOR    TORY    n/a     Original release year
//              TOWN            File owner/licensee
//       TP1    TPE1            Lead performer(s)/Soloist(s)
//       TP2    TPE2            Band/orchestra/accompaniment
//       TP3    TPE3            Conductor/performer refinement
//       TP4    TPE4            Interpreted, remixed, or otherwise modified by
//       TPA    TPOS            Part of a set
//                      TPRO    Produced notice
//       TPB    TPUB            Publisher
//       TRK    TRCK            Track number/Position in set
//       TRD    TRDA    n/a     Recording dates
//              TRSN            Internet radio station name
//              TRSO            Internet radio station owner
//                      TSOA    Album sort order
//                      TSOP    Performer sort order
//                      TSOT    Title sort order
//       TSI    TSIZ    n/a     Size
//       TRC    TSRC            ISRC (international standard recording code)
//       TSS    TSSE            Software/Hardware and settings used for encoding
//       TYE    TYER    n/a     Year
//       TXX    TXXX            User defined text information frame
//       UFI    UFID            Unique file identifier
//              USER            Terms of use
//       ULT    USLT            Unsychronized lyric/text transcription
//       WCM    WCOM            Commercial information
//       WCP    WCOP            Copyright/Legal information
//       WAF    WOAF            Official audio file webpage
//       WAR    WOAR            Official artist/performer webpage
//       WAS    WOAS            Official audio source webpage
//              WORS            Official internet radio station homepage
//              WPAY            Payment
//       WPB    WPUB            Publishers official webpage
//       WXX    WXXX            User defined URL link frame
//
// Information on these frames can be found at:
//
//     ID3v2.2: http://id3.org/id3v2-00
//     ID3v2.3: http://id3.org/d3v2.3.0
//     ID3v2.4: http://id3.org/id3v2.4.0-frames

type Id3v2FrameParser = fn(&mut dyn Bytestream, usize) -> Result<()>;

lazy_static! {
    static ref ID3V2P3_FRAME_PARSERS: 
        HashMap<&'static [u8; 4], Id3v2FrameParser> = {
            let mut m = HashMap::new();
            // m.insert(b"AENC", read_null_frame);
            // m.insert(b"APIC", read_null_frame);
            // m.insert(b"ASPI", read_null_frame);
            // m.insert(b"COMM", read_null_frame);
            // m.insert(b"COMR", read_null_frame);
            // m.insert(b"ENCR", read_null_frame);
            // m.insert(b"EQU2", read_null_frame);
            // m.insert(b"EQUA", read_null_frame);
            // m.insert(b"ETCO", read_null_frame);
            // m.insert(b"GEOB", read_null_frame);
            // m.insert(b"GRID", read_null_frame);
            // m.insert(b"IPLS", read_null_frame);
            // m.insert(b"LINK", read_null_frame);
            // m.insert(b"MCDI", read_null_frame);
            // m.insert(b"MLLT", read_null_frame);
            // m.insert(b"OWNE", read_null_frame);
            // m.insert(b"PCNT", read_null_frame);
            // m.insert(b"POPM", read_null_frame);
            // m.insert(b"POSS", read_null_frame);
            // m.insert(b"PRIV", read_null_frame);
            // m.insert(b"RBUF", read_null_frame);
            // m.insert(b"RVA2", read_null_frame);
            // m.insert(b"RVAD", read_null_frame);
            // m.insert(b"RVRB", read_null_frame);
            // m.insert(b"SEEK", read_null_frame);
            // m.insert(b"SIGN", read_null_frame);
            // m.insert(b"SYLT", read_null_frame);
            // m.insert(b"SYTC", read_null_frame);
            m.insert(b"TALB", read_text_frame as Id3v2FrameParser);
            m.insert(b"TBPM", read_text_frame);
            m.insert(b"TCOM", read_text_frame);
            m.insert(b"TCON", read_text_frame);
            m.insert(b"TCOP", read_text_frame);
            m.insert(b"TDAT", read_text_frame);
            m.insert(b"TDEN", read_text_frame);
            m.insert(b"TDLY", read_text_frame);
            m.insert(b"TDOR", read_text_frame);
            m.insert(b"TDRC", read_text_frame);
            m.insert(b"TDRL", read_text_frame);
            m.insert(b"TDTG", read_text_frame);
            m.insert(b"TENC", read_text_frame);
            m.insert(b"TEXT", read_text_frame);
            m.insert(b"TFLT", read_text_frame);
            m.insert(b"TIME", read_text_frame);
            m.insert(b"TIPL", read_text_frame);
            m.insert(b"TIT1", read_text_frame);
            m.insert(b"TIT2", read_text_frame);
            m.insert(b"TIT3", read_text_frame);
            m.insert(b"TKEY", read_text_frame);
            m.insert(b"TLAN", read_text_frame);
            m.insert(b"TLEN", read_text_frame);
            m.insert(b"TMCL", read_text_frame);
            m.insert(b"TMED", read_text_frame);
            m.insert(b"TMOO", read_text_frame);
            m.insert(b"TOAL", read_text_frame);
            m.insert(b"TOFN", read_text_frame);
            m.insert(b"TOLY", read_text_frame);
            m.insert(b"TOPE", read_text_frame);
            m.insert(b"TORY", read_text_frame);
            m.insert(b"TOWN", read_text_frame);
            m.insert(b"TPE1", read_text_frame);
            m.insert(b"TPE2", read_text_frame);
            m.insert(b"TPE3", read_text_frame);
            m.insert(b"TPE4", read_text_frame);
            m.insert(b"TPOS", read_text_frame);
            m.insert(b"TPRO", read_text_frame);
            m.insert(b"TPUB", read_text_frame);
            m.insert(b"TRCK", read_text_frame);
            m.insert(b"TRDA", read_text_frame);
            m.insert(b"TRSN", read_text_frame);
            m.insert(b"TRSO", read_text_frame);
            m.insert(b"TSIZ", read_text_frame);
            m.insert(b"TSOA", read_text_frame);
            m.insert(b"TSOP", read_text_frame);
            m.insert(b"TSOT", read_text_frame);
            m.insert(b"TSRC", read_text_frame);
            m.insert(b"TSSE", read_text_frame);
            m.insert(b"TXXX", read_text_frame);
            m.insert(b"TYER", read_text_frame);
            // m.insert(b"UFID", read_null_frame);
            // m.insert(b"USER", read_null_frame);
            // m.insert(b"USLT", read_null_frame);
            // m.insert(b"WCOM", read_null_frame);
            // m.insert(b"WCOP", read_null_frame);
            // m.insert(b"WOAF", read_null_frame);
            // m.insert(b"WOAR", read_null_frame);
            // m.insert(b"WOAS", read_null_frame);
            // m.insert(b"WORS", read_null_frame);
            // m.insert(b"WPAY", read_null_frame);
            // m.insert(b"WPUB", read_null_frame);
            // m.insert(b"WXXX", read_null_frame);
            m
        };
}

pub fn read_id3v2p2_frame<B: Bytestream>(reader: &mut B) -> Result<()> {
    let id = reader.read_triple_bytes()?;

    // Is this padding?
    if id == [0, 0, 0] {
        return Ok(());
    }

    let size = reader.read_be_u24()? as usize;
    // Text frame.
    if id[0] == b"T"[0] {
        let encoding = Encoding::parse(reader.read_byte()?);
        let data = reader.read_boxed_slice_bytes(size - 1)?;

        if encoding.is_none() {
            return decode_error("Invalid text encoding.");
        }

        let text = read_text(encoding.unwrap(), &data);

        eprintln!("Frame\t{}\t{}\t{:?}", String::from_utf8_lossy(&id), size, text);
    }
    else {
        eprintln!("Frame\t{}\t{}", String::from_utf8_lossy(&id), size);
        reader.ignore_bytes(size as u64)?;
    }


    Ok(())
}

pub fn read_id3v2p3_frame<B: Bytestream>(reader: &mut B) -> Result<()> {
    let id = reader.read_quad_bytes()?;

    if id == [0, 0, 0, 0] {
        return Ok(());
    }

    let size = reader.read_be_u32()? as usize;
    let flags = reader.read_be_u16()?;

    // Text frame.
    if id[0] == b"T"[0] {
        let encoding = Encoding::parse(reader.read_byte()?);
        let data = reader.read_boxed_slice_bytes(size - 1)?;

        if encoding.is_none() {
            return decode_error("Invalid text encoding.");
        }

        let text = read_text(encoding.unwrap(), &data);

        eprintln!("Frame\t{}\t{}\t{:#b}\t{:?}", String::from_utf8_lossy(&id), size, flags, text);
    }
    else {
        eprintln!("Frame\t{}\t{}\t{:#b}", String::from_utf8_lossy(&id), size, flags);
        reader.ignore_bytes(size as u64)?;
    }

    Ok(())
}

pub fn read_id3v2p4_frame<B: Bytestream + FiniteStream>(reader: &mut B) -> Result<()> {
    let id = reader.read_quad_bytes()?;

    if id == [0, 0, 0, 0] {
        return Ok(());
    }

    let mut size = read_syncsafe_leq32(reader, 28)?;
    let flags = reader.read_be_u16()?;

    // Find a parser for the frame. If there is none, skip over the remainder of the frame as it cannot be parsed.
    let parser = match ID3V2P3_FRAME_PARSERS.get(&id) {
        Some(p) => p,
        None => {
            eprintln!("Frame {:?} is not supported.", String::from_utf8_lossy(&id));

            reader.ignore_bytes(size as u64)?;
            return Ok(());
        }
    };

    if flags & 0x8 == 0x8 {
        reader.ignore_bytes(size as u64)?;
        return unsupported_error("Compression is not supported.");
    }

    if flags & 0x4 == 0x4 {
        reader.ignore_bytes(size as u64)?;
        return unsupported_error("Encryption is not supported.");
    }

    // Frame group identifier.
    if flags & 0x40 == 0x40 {
        reader.read_byte()?;
        size -= 1;
    }

    // The data length indicator is optional in the frame header. This field indicates the original size of the frame
    // body before compression, encryption, and/or unsynchronisation. It is mandatory if encryption or compression are 
    // used, but only encouraged for unsynchronisation.
    let data_len_indicator = if flags & 0x1 == 0x1 { 
        size -= 4;
        Some(read_syncsafe_leq32(reader, 28)?)
    } 
    else { 
        None
    };

    let unsynchronised = flags & 0x2 == 0x2;

    // The frame body is unsynchronised.
    if unsynchronised {
        // The original frame body size is known, the frame body can be streamed using an UnsyncStream to decode 
        // the unsynchronisation scheme on the fly.
        if let Some(data_len) = data_len_indicator {
            parser(&mut UnsyncStream::new(reader), data_len as usize)?;
        }
        // The original frame body size is NOT known. Unfortunately, the length of the content in many frames is 
        // implicitly determined via the original frame body size. However, in this case we only have the encoded 
        // frame body size which may be smaller or larger than the original body size depending on the result of 
        // encryption, compression, and/or unsynchronisation. Therefore, we must decode the encoded frame body 
        // into memory before passing it to the frame parser. Hopefully this is not a common occurence.
        else {
            let mut encoded = reader.read_boxed_slice_bytes(size as usize)?;
            let decoded = decode_unsynchronisation(&mut encoded);
            parser(&mut BufStream::new(decoded), decoded.len())?;
        }
    }
    // The frame body has not been unsynchronised.
    else {
        parser(reader, size as usize)?;
    };


    Ok(())
}

fn read_text_frame(reader: &mut dyn Bytestream, data_size: usize) -> Result<()> {
    let encoding = Encoding::parse(reader.read_byte()?);
    let data = reader.read_boxed_slice_bytes(data_size - 1)?;

    if encoding.is_none() {
        return decode_error("Invalid text encoding.");
    }

    eprintln!("{:?}", read_text(encoding.unwrap(), &data));
    
    Ok(())
}

enum Encoding {
    Iso8859_1,
    Utf16Bom,
    Utf16Be,
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
            _ => None
        }
    }
}

fn read_text(encoding: Encoding, data: &[u8]) -> Vec<String> {
    match encoding {
        Encoding::Iso8859_1 => read_text_utf8(data),
        Encoding::Utf8      => read_text_utf8(data),
        Encoding::Utf16Bom  => read_text_utf16(data),
        Encoding::Utf16Be   => read_text_utf16(data),
    }
}

fn read_text_utf16(data: &[u8]) -> Vec<String> {
    let (text, _encoding, _errors) = UTF_16BE.decode(data);

    text.split_terminator('\0')
        .filter_map(|line| {
            // Remove empty strings that may exist due to extra null characters.
            if line.len() > 0 { Some(line.to_string()) } else { None }
        })
        .collect::<Vec<_>>()
}

fn read_text_utf8(data: &[u8]) -> Vec<String> {
    // UTF-8 is endian-agnostic so the data slice may be converted directly into a UTF-8 string. However, there may be
    // multiple related, yet independant, strings recorded in data. Iterate over each independant string by splitting 
    // at the null terminator and collect them into a vector.
    String::from_utf8_lossy(data)
        .split_terminator('\0')
        .filter_map(|line| {
            // Remove empty strings that may exist due to extra null characters.
            if line.len() > 0 { Some(line.to_string()) } else { None }
        })
        .collect::<Vec<_>>()
}
