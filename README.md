# Client

This application implements a grpc client with interrupt-driven async
monitors for reading events on digital-in on a GNU/Linux system and
transmitting them securely (TLS) to a server. In addition, the
following data is sent:

- GPS location (at boot)
- heartbeat containing the client ID (at some regular interval)

The following responses can be handled:

- Carry on: continue listening for new data
- Software update: download a new version of the client from a predefined location
- Config update: download a new configuration file for the client
- Exit: terminate the application with exit code success

Sleep time after connection retries, server host name, gpiochip, and
the number of ports are configurable in a conf.toml file.

## Building for ARM32 on Debian GNU/Linux

TODO: add required packages

Run the following commands:

```
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=/usr/bin/arm-linux-gnueabihf-gcc

cargo build --release --target=armv7-unknown-linux-gnueabihf
```
