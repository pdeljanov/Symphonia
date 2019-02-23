# Sonata Play

Quick-and-dirty audio player for testing Sonata decoders.

## Operating System Support

Sonata Play currently only supports Linux with PulseAudio.

## Usage

```
# Play a song
cargo run -- /path/to/the/file

# Probe a file for streams
cargo run -- --probe /path/to/the/file

# Decode and check if the decoded audio is valid (some formats)
cargo run -- --check /path/to/the/file
```

## License

Sonata is provided under the LGPLv2.1 license. Please refer to the LICENSE file for more details.

## Contributing

Sonata is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.
