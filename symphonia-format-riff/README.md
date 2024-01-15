# Symphonia RIFF (AIFF, AVI, WAVE) Demuxer

[![Docs](https://docs.rs/symphonia-format-riff/badge.svg)](https://docs.rs/symphonia-format-riff)

AIFF/AVI/WAVE demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

This crate supports demuxing media containers based off the Resource Interchange File Format (RIFF). Specific format support may be enabled or disabled using feature flags. However, by default, all formats are enabled.

| Format | Feature Flag | Default |
|--------|--------------|---------|
| AIFF   | `aiff`       | Yes     |
| WAVE   | `wav`        | Yes     |

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.