# Symphonia DSD Format Demuxer

Pure Rust demuxer for DSD (Direct Stream Digital) audio formats.

## Supported Formats

- **DSF** (DSD Stream File) - Sony's DSD format ✅ Implemented
- **DFF** (DSDIFF - DSD Interchange File Format) - Philips/Sony IFF-based format ✅ Implemented

## Features

- Native DSD output support (DsdU8, DsdU16, DsdU32)
- Support for DSD64, DSD128, DSD256, DSD512, DSD1024
- Multi-channel support (stereo, 5.1, etc.)
- Metadata extraction from ID3v2 tags (DSF) and INFO chunks (DFF)

## Usage

This crate is part of the Symphonia project and is designed to be used with the `symphonia` meta-crate.

## License

This project is licensed under the Mozilla Public License 2.0.
