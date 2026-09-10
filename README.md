<p align="center"><img src="docs/assets/senda-link.png" alt="Senda Link" width="128" height="128"></p>
<h1 align="center">Senda Link</h1>
<p align="center">A virtual audio cable for macOS.</p>

Senda Link adds five virtual audio devices to your Mac, with 2, 8, 16, 32 and 128 channels.
Route audio from one application into another by picking a Senda Link device as the output
in one and the input in the other.

- 44.1, 48, 88.2, 96, 176.4 and 192 kHz, 32-bit float
- Bit-transparent: samples pass through unchanged
- Apple silicon, macOS 13 or later
- No third-party dependencies

## Install

Download the installer from [Releases](../../releases). It installs the driver and restarts
the system audio service, which briefly interrupts audio in other applications.

## Build from source

Needs a stable Rust toolchain and Xcode Command Line Tools.

    cargo test --workspace
    cargo xtask bundle          # target/SendaLink.driver

A driver built from source is unsigned, so macOS will only load it with reduced security
settings. To install it by hand:

    sudo cp -R target/SendaLink.driver /Library/Audio/Plug-Ins/HAL/ && sudo killall coreaudiod

To remove it:

    sudo rm -rf /Library/Audio/Plug-Ins/HAL/SendaLink.driver && sudo killall coreaudiod

## Licence

Apache 2.0, copyright South Loop Studios Limited. See [LICENSE](LICENSE). Contributions are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).
