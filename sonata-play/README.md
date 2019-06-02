# Sonata Play

Quick-and-dirty audio player for testing Sonata demuxers and decoders.

## Operating System Support

Sonata Play currently only supports Linux with PulseAudio.

## Usage

```bash
# Play an audio file.
cargo run -- /path/to/file

# Play an audio file and verify the decoded audio whilst playing.
cargo run -- --verify /path/to/file

# Seek the audio file to the desired timestamp and then play.
cargo run -- -s <seconds> /path/to/file

# Probe a file for metadata, streams, visuals, etc.
cargo run -- --probe-only /path/to/file

# Decode and verify if the decoded audio is valid (some formats only).
cargo run -- --verify-only /path/to/file

# Decode, but do not play or verify the decoded audio (benchmarking).
cargo run -- --decode-only /path/to/file
```

## License

Sonata is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Sonata is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.
