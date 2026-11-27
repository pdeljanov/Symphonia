//! Tests of [`crate::packet`].

use super::*;

struct Packet {
    toc_byte: u8,
    vbr: bool,
    padding_flag: bool,
    frame_count: Option<u8>,
    padding_length: usize,
    frames: Vec<Vec<u8>>,
}

impl Packet {
    fn new(toc_byte: u8) -> Self {
        return Self {
            toc_byte,
            vbr: false,
            padding_flag: false,
            frame_count: None,
            padding_length: 0,
            frames: Vec::new(),
        };
    }

    fn vbr(mut self, vbr: bool) -> Self {
        self.vbr = vbr;
        return self;
    }

    fn frame_count(mut self, frame_count: u8) -> Self {
        self.frame_count = Some(frame_count);
        return self;
    }

    fn padding(mut self, padding_length: usize) -> Self {
        self.padding_flag = true;
        self.padding_length = padding_length;
        return self;
    }

    fn add_frame(mut self, frame: &[u8]) -> Self {
        self.frames.push(frame.to_vec());
        return self;
    }

    fn build(self) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(self.toc_byte);

        if let Some(frame_count) = self.frame_count {
            let mut frame_count_byte = 0u8;
            if self.vbr {
                frame_count_byte |= 0b1000_0000;
            }
            if self.padding_flag {
                frame_count_byte |= 0b0100_0000;
            }
            frame_count_byte |= frame_count & 0b00111111;
            data.push(frame_count_byte);
        }

        if self.padding_flag {
            let mut remaining_padding = self.padding_length;
            while remaining_padding >= 255 {
                data.push(255u8);
                remaining_padding -= MAX_PADDING_VALUE as usize;
            }
            data.push(remaining_padding as u8);
        }

        if self.vbr {
            // Every frame but the last carries its length; the last takes the remaining bytes.
            for frame in self.frames.iter().take(self.frames.len().saturating_sub(1)) {
                let frame_len = frame.len();
                assert!(frame_len < 252, "Frame length too long for test builder (max 251)");
                data.push(frame_len as u8);
            }
        }
        for frame in self.frames {
            data.extend(frame);
        }

        if self.padding_flag {
            data.extend(vec![0u8; self.padding_length]);
        }

        return data;
    }
}

#[test]
fn single_frame_packet() {
    let toc_byte = 0b0000_0000;
    let frame_data = [0xAA, 0xBB, 0xCC];

    let packet_data = Packet::new(toc_byte).add_frame(&frame_data).build();

    let packet = FramePacket::new(&packet_data).expect("Failed to parse single-frame packet");

    assert_eq!(packet.frames.len(), 1);
    assert_eq!(packet.frames[0], &frame_data[..]);
}

#[test]
fn two_equal_frames_packet() {
    let toc_byte = 0b0000_0001;
    let frame_data = [0xAA, 0xBB, 0xCC, 0xDD];

    let packet_data = Packet::new(toc_byte).add_frame(&frame_data).build();

    let packet = FramePacket::new(&packet_data).expect("Failed to parse two equal frames packet");

    assert_eq!(packet.frames.len(), 2);
    assert_eq!(packet.frames[0], &frame_data[0..2]);
    assert_eq!(packet.frames[1], &frame_data[2..4]);
}

#[test]
fn two_different_frames_packet() {
    let toc_byte = 0b0000_0010;
    let frame_1 = [0xAA, 0xBB];
    let frame_2 = [0xCC, 0xDD, 0xEE];

    let frame_1_len = frame_1.len();
    assert!(frame_1_len < 252, "Frame length too long for test builder (max 251)");
    let frame_1_len_byte = [frame_1_len as u8];

    let data = Packet::new(toc_byte)
        .add_frame(&frame_1_len_byte)
        .add_frame(&frame_1)
        .add_frame(&frame_2)
        .build();

    let packet = FramePacket::new(&data).expect("Failed to parse two different frames packet");

    assert_eq!(packet.frames.len(), 2);
    assert_eq!(packet.frames[0], &frame_1[..]);
    assert_eq!(packet.frames[1], &frame_2[..]);
}

#[test]
fn arbitrary_frames_cbr_packet() {
    let toc_byte = 0b0000_0011;
    let frame_count = 3u8;

    let frame_1 = [0xAA, 0xBB];
    let frame_2 = [0xCC, 0xDD];
    let frame_3 = [0xEE, 0xFF];

    let data = Packet::new(toc_byte)
        .frame_count(frame_count)
        .add_frame(&frame_1)
        .add_frame(&frame_2)
        .add_frame(&frame_3)
        .build();

    let packet = FramePacket::new(&data).expect("Failed to parse arbitrary frames CBR packet");

    assert_eq!(packet.frames.len(), 3);
    assert_eq!(packet.frames[0], &frame_1[..]);
    assert_eq!(packet.frames[1], &frame_2[..]);
    assert_eq!(packet.frames[2], &frame_3[..]);
}

#[test]
fn arbitrary_frames_vbr_packet() {
    let toc_byte = 0b0000_0011;
    let frame_count = 2u8;

    let frame_1 = [0xAA, 0xBB];
    let frame_2 = [0xCC, 0xDD, 0xEE];

    let data = Packet::new(toc_byte)
        .vbr(true)
        .frame_count(frame_count)
        .add_frame(&frame_1)
        .add_frame(&frame_2)
        .build();

    let packet = FramePacket::new(&data).expect("Failed to parse arbitrary frames VBR packet");

    assert_eq!(packet.frames.len(), 2);
    assert_eq!(packet.frames[0], &frame_1[..]);
    assert_eq!(packet.frames[1], &frame_2[..]);
}

#[test]
fn packet_with_padding() {
    let toc_byte = 0b0000_0011;
    let frame_count = 1u8;
    let frame = [0xAA, 0xBB, 0xCC];

    let data = Packet::new(toc_byte).frame_count(frame_count).padding(2).add_frame(&frame).build();

    let packet = FramePacket::new(&data).expect("Failed to parse packet with padding");

    assert_eq!(packet.frames.len(), 1);
    assert_eq!(packet.frames[0], &frame[..]);
}

#[test]
fn handle_invalid_packet_length() {
    let toc_byte = 0b0000_0001;
    let frame_data = [0xAA, 0xBB, 0xCC];

    let data = Packet::new(toc_byte).add_frame(&frame_data).build();

    let result = FramePacket::new(&data);

    assert_eq!(result.err(), Some(Error::OddCode1Payload));
}

#[test]
fn total_duration_limits_all_frame_sizes() {
    // (config_index, max_valid_frames_for_120ms)
    // Config 16 = 2.5 ms (CELT NB) -> max 48 frames
    // Config 17 = 5 ms   (CELT NB) -> max 24 frames
    // Config 0  = 10 ms  (SILK NB) -> max 12 frames
    // Config 1  = 20 ms  (SILK NB) -> max 6 frames
    // Config 2  = 40 ms  (SILK NB) -> max 3 frames
    // Config 3  = 60 ms  (SILK NB) -> max 2 frames
    let cases: [(u8, u8); 6] = [(16, 48), (17, 24), (0, 12), (1, 6), (2, 3), (3, 2)];

    for (config, max_frames) in cases {
        let toc_byte = (config << 3) | 0b011; // Code 3

        // Exactly 120 ms -> valid
        let mut valid_builder = Packet::new(toc_byte).frame_count(max_frames);
        for _ in 0..max_frames {
            valid_builder = valid_builder.add_frame(&[0xAA]);
        }
        let valid_data = valid_builder.build();
        let valid_packet =
            FramePacket::new(&valid_data).expect("Packet at 120 ms boundary should be valid");
        assert_eq!(valid_packet.frames.len(), max_frames as usize);

        // Exceeding 120 ms (max_frames + 1) -> ExcessiveTotalDuration error
        let invalid_count = max_frames + 1;
        let mut invalid_builder = Packet::new(toc_byte).frame_count(invalid_count);
        for _ in 0..invalid_count {
            invalid_builder = invalid_builder.add_frame(&[0xAA]);
        }
        let invalid_data = invalid_builder.build();
        let invalid_res = FramePacket::new(&invalid_data);
        assert!(
            invalid_res.is_err(),
            "Config {config} with {invalid_count} frames should exceed 120 ms"
        );
    }
}

#[test]
fn packet_with_extended_padding() {
    let toc_byte = 0b0000_0011;
    let frame_count = 1u8;
    let frame = [0xAA, 0xBB];

    let data =
        Packet::new(toc_byte).frame_count(frame_count).padding(300).add_frame(&frame).build();

    let packet = FramePacket::new(&data).expect("Failed to parse packet with 300-byte padding");
    assert_eq!(packet.frames.len(), 1);
    assert_eq!(packet.frames[0], &frame[..]);
    return;
}

#[test]
fn code0_and_code1_zero_byte_dtx_frames() {
    let code0_dtx = [0x00u8];
    let pkt0 = FramePacket::new(&code0_dtx).expect("1-byte Code 0 DTX packet should be valid");
    assert_eq!(pkt0.frames.len(), 1);
    assert!(pkt0.frames[0].is_empty());

    let code1_dtx = [0x01u8];
    let pkt1 = FramePacket::new(&code1_dtx).expect("1-byte Code 1 DTX packet should be valid");
    assert_eq!(pkt1.frames.len(), 2);
    assert!(pkt1.frames[0].is_empty());
    assert!(pkt1.frames[1].is_empty());

    assert!(FramePacket::new(&[0x02u8]).is_err());
    assert!(FramePacket::new(&[0x03u8]).is_err());
    return;
}

#[test]
fn self_delimited_multistream_packets() {
    let mut stream_buf = Vec::new();

    // Stream 0: Code 0 self-delimited (TOC=0x00, len=3, payload=[0x11, 0x22, 0x33])
    let s0_start = stream_buf.len();
    stream_buf.extend_from_slice(&[0x00, 3, 0x11, 0x22, 0x33]);
    let s0_expected_len = stream_buf.len() - s0_start;

    // Stream 1: Code 1 self-delimited (TOC=0x01, len=2 per frame, payloads=[0x44, 0x55], [0x66, 0x77])
    let s1_start = stream_buf.len();
    stream_buf.extend_from_slice(&[0x01, 2, 0x44, 0x55, 0x66, 0x77]);
    let s1_expected_len = stream_buf.len() - s1_start;

    // Stream 2: Code 2 self-delimited (TOC=0x02, n1=2, n2=3, payloads=[0x88, 0x99], [0xAA, 0xBB, 0xCC])
    let s2_start = stream_buf.len();
    stream_buf.extend_from_slice(&[0x02, 2, 3, 0x88, 0x99, 0xAA, 0xBB, 0xCC]);
    let s2_expected_len = stream_buf.len() - s2_start;

    // Stream 3: Code 3 CBR self-delimited with 2 bytes trailing padding
    // TOC=0x03, ch=(padding=1, m=3) = 0x43, pad_len=2, cbr_size=2, 3 frames of 2 bytes, 2 pad bytes
    let s3_start = stream_buf.len();
    stream_buf
        .extend_from_slice(&[0x03, 0x43, 2, 2, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0xF0, 0xF1]);
    let s3_expected_len = stream_buf.len() - s3_start;

    // Stream 4: Code 3 VBR self-delimited with 3 bytes trailing padding
    // TOC=0x03, ch=(vbr=1, padding=1, m=2) = 0xC2, pad_len=3, n1=1, n2=3, frames=[0x10], [0x20, 0x30, 0x40], pad=[0, 0, 0]
    let s4_start = stream_buf.len();
    stream_buf.extend_from_slice(&[0x03, 0xC2, 3, 1, 3, 0x10, 0x20, 0x30, 0x40, 0x00, 0x00, 0x00]);
    let s4_expected_len = stream_buf.len() - s4_start;

    // Final undelimited stream at end: Code 0 [0x00, 0x9A, 0xBC]
    stream_buf.extend_from_slice(&[0x00, 0x9A, 0xBC]);

    let mut cursor = &stream_buf[..];

    let (p0, c0) = FramePacket::parse_self_delimited(cursor).expect("Stream 0 failed");
    assert_eq!(c0, s0_expected_len);
    assert_eq!(p0.frames, vec![&[0x11, 0x22, 0x33][..]]);
    cursor = &cursor[c0..];

    let (p1, c1) = FramePacket::parse_self_delimited(cursor).expect("Stream 1 failed");
    assert_eq!(c1, s1_expected_len);
    assert_eq!(p1.frames, vec![&[0x44, 0x55][..], &[0x66, 0x77][..]]);
    cursor = &cursor[c1..];

    let (p2, c2) = FramePacket::parse_self_delimited(cursor).expect("Stream 2 failed");
    assert_eq!(c2, s2_expected_len);
    assert_eq!(p2.frames, vec![&[0x88, 0x99][..], &[0xAA, 0xBB, 0xCC][..]]);
    cursor = &cursor[c2..];

    let (p3, c3) = FramePacket::parse_self_delimited(cursor).expect("Stream 3 failed");
    assert_eq!(c3, s3_expected_len);
    assert_eq!(p3.frames, vec![&[0x01, 0x02][..], &[0x03, 0x04][..], &[0x05, 0x06][..]]);
    cursor = &cursor[c3..];

    let (p4, c4) = FramePacket::parse_self_delimited(cursor).expect("Stream 4 failed");
    assert_eq!(c4, s4_expected_len);
    assert_eq!(p4.frames, vec![&[0x10][..], &[0x20, 0x30, 0x40][..]]);
    cursor = &cursor[c4..];

    let p_final = FramePacket::new(cursor).expect("Final stream failed");
    assert_eq!(p_final.frames, vec![&[0x9A, 0xBC][..]]);
    return;
}

/// RFC 6716 Section 3.2.5: a set padding flag requires at least one padding length byte, and a
/// padding length byte of 255 requires a following byte, in both framings.
#[test]
fn code3_padding_flag_without_a_complete_padding_length_is_rejected() {
    assert!(FramePacket::new(&[0b0000_0011, 0b0100_0001]).is_err());
    assert!(FramePacket::parse_self_delimited(&[0b0000_0011, 0b0100_0001]).is_err());
    assert!(FramePacket::new(&[0b0000_0011, 0b0100_0001, 0xFF]).is_err());
    assert!(FramePacket::parse_self_delimited(&[0b0000_0011, 0b0100_0001, 0xFF]).is_err());
    return;
}

/// Converting a framing error into a `symphonia_core` error must not allocate or leak.
#[test]
fn conversion_to_core_error_uses_a_static_message() {
    let err: symphonia_core::errors::Error = Error::TooShort.into();
    assert!(matches!(
        err,
        symphonia_core::errors::Error::DecodeError(
            "opus: packet shorter than its framing requires"
        )
    ));
    return;
}
/// The frame list is bounded by RFC 6716 Section 3.4 [R5]; a 49th push is refused.
#[test]
fn frame_list_refuses_a_49th_frame() {
    let mut frames = Frames::new();
    for _ in 0..MAX_FRAMES {
        frames.push(&[]).unwrap();
    }
    assert_eq!(frames.push(&[]), Err(Error::TooManyFrames));
    assert_eq!(frames.len(), MAX_FRAMES);
}

impl<'a> PartialEq<Vec<&'a [u8]>> for Frames<'a> {
    fn eq(&self, other: &Vec<&'a [u8]>) -> bool {
        return self.as_slice() == other.as_slice();
    }
}
