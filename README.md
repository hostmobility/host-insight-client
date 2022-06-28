# Client

## Building for ARM32 on Debian GNU/Linux

TODO: add required packages

Run the following commands:

export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=/usr/bin/arm-linux-gnueabihf-gcc
cargo build --release --target=armv7-unknown-linux-gnueabihf
