# Symphonia Check

A utility to test the output of a file decoded by Symphonia against `ffmpeg`.

## Prerequisites

`ffmpeg` must be installed and present in your `PATH`.

## Usage

```bash
# Test a file, printing information on every packet with an error (no extra arguments).
symphonia-check /path/to/file

# Test a file, printing information on every sample with an error (--samples).
symphonia-check --samples /path/to/file

# Test a file, only printing the final test results (-q/--quiet).
symphonia-check -q /path/to/file

# Test a file, and abort the test on the first failed packet (-f/--first-fail).
symphonia-check -f /path/to/file
```

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.
