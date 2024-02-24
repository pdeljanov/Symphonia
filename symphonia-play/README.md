# Symphonia Play

A simple audio player for testing Project Symphonia demuxers and decoders.

## Usage

```bash
# Play an audio file.
symphonia-play /path/to/file

# Play an audio file and verify the decoded audio whilst playing (some formats only).
symphonia-play --verify /path/to/file

# Seek the audio file to the desired timestamp and then play.
symphonia-play -s <seconds> /path/to/file

# Play a specific track within the file.
symphonia-play -t <track> /path/to/file

# Probe a file for streams and metadata (tags, visuals, etc.)
symphonia-play --probe-only /path/to/file

# Decode and verify if the decoded audio is valid, but do not play it (some formats only).
symphonia-play --verify-only /path/to/file

# Decode, but do not play or verify the decoded audio (benchmarking).
symphonia-play --decode-only /path/to/file

# Do any of the above, but get the encoded audio from standard input by using '-' as the file path.
cat /path/to/file | symphonia-play -
curl -s https://radio.station.com/stream | symphonia-play -
youtube-dl -f 140 <url> -o - | symphonia-play -
yt-dlp -f 140 <url> -o - | symphonia-play -
```

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/tree/master/CONTRIBUTING.md).
