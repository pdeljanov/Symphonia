# Symphonia Play

A quick-and-dirty audio player for testing Symphonia demuxers and decoders.

## Operating System Support

Symphonia Play currently only supports *audio output* on Linux with PulseAudio. All other features
are cross-platform.

## Usage

```bash
# Play an audio file.
symphonia-play /path/to/file

# Play an audio file and verify the decoded audio whilst playing (some formats only).
symphonia-play --verify /path/to/file

# Seek the audio file to the desired timestamp and then play.
symphonia-play -s <seconds> /path/to/file

# Probe a file for streams and metadata (tags, visuals, etc.)
symphonia-play --probe-only /path/to/file

# Decode and verify if the decoded audio is valid, but do not play it (some formats only).
symphonia-play --verify-only /path/to/file

# Decode, but do not play or verify the decoded audio (benchmarking).
symphonia-play --decode-only /path/to/file

# Do any of the above, but get the encoded audio from standard input by using '-' as the file path.
cat /path/to/file | symphonia-play -
curl -s https://radio.station.com/stream | symphonia-play -

```

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.
