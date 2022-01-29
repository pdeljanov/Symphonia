# Symphonia Check

A utility to test the output of a file decoded by Symphonia against `ffmpeg` and other reference decoders.

The currently supported reference decoders include:

* `ffmpeg`
* `flac`
* `mpg123` (when provided by `libmad` aka. `mpg321`)
* `oggdec`

## Prerequisites

The reference decoder must must be installed and present in your `PATH`.

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

# Any of the above commands, without gapless playback enabled (--no-gapless).
symphonia-check --no-gapless /path/to/file

# Any of the above commands, using a specific reference decoder (--ref <decoder>).
symphonia-check --ref flac /path/to/flac/file
```

### Interpreting Results

Most files will pass, however, `symphonia-check` is a very simple tool, and a failure **does not** necessarily mean an invalid decoding. All decoders, including the reference decoders, contain bugs that can cause differences when tested against Symphonia.

Some scenarios can result in `symphonia-check` reporting large errors on almost all samples, yet when played sound okay. These scenarios are almost always false positives that can be caused by:

* The reference decoder or Symphonia dropping differing amounts of samples when encountering corruption.
* Testing a Symphonia decoder that does not support gapless playback without the `--no-gapless` flag.

In the first scenario, it is useful to verify the file decodes without any warnings from `ffmpeg`.

```bash
ffmpeg -v debug -i /path/to/file -f null -
```

Regardless, feel free to open an issue if you encounter a check failure. Please note that a sample file reproducing the failure is almost always required to triage the issue.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

All contributors will be credited within the CONTRIBUTORS file.
