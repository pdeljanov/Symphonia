# Symphonia DSD Codec

Pure Rust decoder for DSD (Direct Stream Digital) audio.

## Features

- Native DSD output (pass-through for cpal DsdU8/16/32)
- Support for DSD64, DSD128, DSD256, DSD512, DSD1024
- Multi-channel support

## Decoder Strategy

This decoder implements a **native DSD pass-through** approach. DSD data is passed directly to the audio output without conversion to PCM. This requires an audio output system that supports native DSD (such as cpal with DSD support).

For outputs that don't support native DSD, DSD-to-PCM conversion would be required (not currently implemented).

## Usage

This crate is part of the Symphonia project and is designed to be used with the `symphonia` meta-crate.

## License

This project is licensed under the Mozilla Public License 2.0.
