use symphonia_core::checksum::Crc32;
use symphonia_core::errors::{Error, Result};
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::{MediaSourceStream, Monitor};
use symphonia_format_ogg::OggReader;

const FIRST_PAGE: u8 = 0x02;
const LAST_PAGE: u8 = 0x04;

/// Builds a page holding a single packet shorter than 255 bytes.
fn page(serial: u32, sequence: u32, header_type: u8, granule: u64, packet: &[u8]) -> Vec<u8> {
    let mut page = Vec::new();

    // Capture pattern, version and header type.
    page.extend_from_slice(b"OggS");
    page.push(0);
    page.push(header_type);

    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());

    // CRC, computed below over the page with this field zeroed.
    page.extend_from_slice(&[0; 4]);

    // One segment holding the whole packet.
    page.push(1);
    page.push(u8::try_from(packet.len()).expect("a packet shorter than 255 bytes"));
    page.extend_from_slice(packet);

    let mut crc = Crc32::new(0);
    crc.process_buf_bytes(&page);
    page[22..26].copy_from_slice(&crc.crc().to_le_bytes());

    page
}

/// Builds an Opus link of two 20 ms packets. Returns its pages separately.
fn opus_link(serial: u32) -> Vec<Vec<u8>> {
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, 2, 0, 0]);
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&[0, 0, 0]);

    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&[0; 8]);

    // TOC configuration 31, one frame: 20 ms of CELT fullband.
    let audio = [0xf8, 0x00];

    vec![
        page(serial, 0, FIRST_PAGE, 0, &head),
        page(serial, 1, 0, 0, &tags),
        page(serial, 2, 0, 960, &audio),
        page(serial, 3, LAST_PAGE, 1920, &audio),
    ]
}

/// What reading a whole file produced.
struct Outcome {
    packets: usize,
    resets: usize,
    end: Result<()>,
}

/// Reads every packet, continuing through resets.
fn read_all(data: Vec<u8>) -> Outcome {
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default());

    let mut reader =
        OggReader::try_probe_new(mss, Default::default()).expect("a readable first link");

    let mut packets = 0;
    let mut resets = 0;

    loop {
        match reader.next_packet() {
            Ok(Some(_)) => packets += 1,
            Ok(None) => return Outcome { packets, resets, end: Ok(()) },
            Err(Error::ResetRequired) => resets += 1,
            Err(err) => return Outcome { packets, resets, end: Err(err) },
        }
    }
}

fn is_unexpected_eof(result: &Result<()>) -> bool {
    matches!(result, Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof)
}

#[test]
fn chain_reads_both_links() {
    let data = [opus_link(1), opus_link(2)].concat().concat();

    let outcome = read_all(data);

    assert_eq!((outcome.packets, outcome.resets), (4, 1));
    assert!(outcome.end.is_ok());
}

#[test]
fn single_link_ends_cleanly() {
    let outcome = read_all(opus_link(1).concat());

    assert_eq!((outcome.packets, outcome.resets), (2, 0));
    assert!(outcome.end.is_ok());
}

#[test]
fn chain_cut_while_starting_second_link_is_unexpected_eof() {
    let mut data = [opus_link(1), opus_link(2)].concat().concat();

    // Keep the second link's first page and half of its comment page.
    let second = opus_link(2);
    let cut = opus_link(1).concat().len() + second[0].len() + second[1].len() / 2;
    data.truncate(cut);

    let outcome = read_all(data);

    assert_eq!(outcome.packets, 2);
    assert!(is_unexpected_eof(&outcome.end), "{:?}", outcome.end);
}
