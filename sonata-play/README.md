# Sonata Play

Quick-and-dirty audio player for testing Sonata decoders.

## Operating System Support

Sonata Play currently only supports Linux with PulseAudio.

## Usage

```
# Play an audio file.
cargo run -- /path/to/file

# Play an audio file and verify the decoded while playing.
cargo run -- --verify /path/to/file

# Seek to desired location, and play the audio file.
cargo run -- -s <seconds> /path/to/file

# Probe a file for audio streams.
cargo run -- --probe-only /path/to/file

# Decode and verify if the decoded audio is valid (some formats only).
cargo run -- --verify-only /path/to/file

# Decode but do not play or verify the decode audio (benchmarking).
cargo run -- --decode-only /path/to/file
```

## License

Sonata is provided under the LGPLv2.1 license. Please refer to the LICENSE file for more details.

## Contributing

Sonata is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.
