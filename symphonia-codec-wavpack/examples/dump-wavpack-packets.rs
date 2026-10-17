//! Demonstrates dumping wavpack packet info and various operations


use std::env;
use std::fs::File;
use std::process;

use symphonia_codec_wavpack::WavPackReader;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::Result;
use symphonia_core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::units::Timestamp;

fn main() {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: dump-wavpack-packets <file.wv>");
        process::exit(1);
    };

    let file = match File::open(&path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("open {path}: {err}");
            process::exit(1);
        }
    };

    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = match WavPackReader::try_new(mss, FormatOptions::default()) {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("try_new: {err}");
            process::exit(1);
        }
    };

    let info = reader.format_info();
    println!("format: {} ({})", info.long_name, info.short_name);

    for track in reader.tracks() {
        print!("track {}: ", track.id);
        match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(params)) => {
                println!(
                    "codec={:?} rate={:?} format={:?} bits={:?} channels={:?}",
                    params.codec,
                    params.sample_rate,
                    params.sample_format,
                    params.bits_per_sample,
                    params.channels.as_ref().map(|ch| ch.count()),
                );
            }
            other => println!("{other:?}"),
        }
    }

    // dump packet info
    let end_ts = match dump_packets(&mut reader) {
        Ok(end_ts) => end_ts,
        Err(err) => {
            eprintln!("next_packet: {err}");
            process::exit(1);
        }
    };

    // jump back to half of track
    let required_ts = Timestamp::new((end_ts / 2) as i64);
    let seeked_to = match reader.seek(
        SeekMode::Accurate,
        SeekTo::TimeStamp { ts: required_ts, track_id: 0 },
    ) {
        Ok(seeked_to) => seeked_to,
        Err(err) => {
            eprintln!("seek: {err}");
            process::exit(1);
        }
    };
    println!(
        "seek: required={} actual={}",
        seeked_to.required_ts.get(),
        seeked_to.actual_ts.get(),
    );

    // dump packet info again
    if let Err(err) = dump_packets(&mut reader) {
        eprintln!("next_packet after seek: {err}");
        process::exit(1);
    }
}

fn dump_packets(reader: &mut WavPackReader<'_>) -> Result<u64> {
    let mut packet_count = 0;
    let mut end_ts = 0;

    loop {
        match reader.next_packet() {
            Ok(Some(packet)) => {
                packet_count += 1;
                end_ts = packet.pts().get() as u64 + packet.dur().get();
                println!(
                    "packet {packet_count}: track={} pts={} dur={} bytes={}",
                    packet.track_id(),
                    packet.pts().get(),
                    packet.dur().get(),
                    packet.buf().len(),
                );
            }
            Ok(None) => break,
            Err(err) => return Err(err),
        }
    }

    println!("packets: {packet_count}");
    Ok(end_ts)
}
