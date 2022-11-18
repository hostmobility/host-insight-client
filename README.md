# Client

This application implements a grpc client with interrupt-driven async
monitors for reading events on digital-in and/or CAN frames on a
GNU/Linux system and then transmitting them securely (TLS) to a grpc
server. CAN frames are decoded using a DBC file and sent in
a human-readable format.

In addition, the following data is sent:

- GPS location (at boot)
- heartbeat containing a status code (at some regular interval)

All requests contain the client ID in the header.

The following responses can be handled:

- Carry on: continue listening for new data
- Software update: download a new version of the client from a predefined location
- Config update: download a new configuration file for the client
- Exit: terminate the application with exit code success

Host requirements:

- CAN bus and/or digital inputs
- glibc v2.28 or later
- OpenSSL

## CAN

The following signal values are supported:

- signed integers
- unsigned integers
- floats including from extended value type list
- strings (enums) from value descriptions

CAN timestamps are not yet implemented. There is experimental support
for multiplexed signals.

## Digital in

Each digital input is given both an internal and an external name. The
former is used for finding an existing port on the device and the
latter for communicating a function to the server.

## Example configuration

The application will look for a conf.toml file firstly in
~/.config/ada-client/ and secondly in /etc/opt/ada-client/.

Example configuration that enables six digital-in and two CAN ports:

```
uid = "42"

[digital_in]
ports = [ { internal_name = "digital-in-0", external_name = "Door" },
          { internal_name = "digital-in-1", external_name = "Light" },
          { internal_name = "digital-in-2", external_name = "Finger protection" }]


[can]
ports = [ { name = "can0", bitrate = 125000, listen_only = true  },
          { name = "can1", bitrate = 500000, listen_only = false } ]
dbc_file = "sample.dbc"

[time]
sleep_min_s = 1
sleep_max_s = 36000
heartbeat_m = 60

[server]
address = "example.hostmobility.org"

[position]
longitude = 12.013
latitude = 57.674
```

## Building for ARM32 on Debian GNU/Linux

Install build dependencies:

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add armv7-unknown-linux-gnueabihf
sudo apt install gcc-arm-linux-gnueabihf linux-libc-dev-armhf-cross
```

Use ARMv7 linker:

```
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=/usr/bin/arm-linux-gnueabihf-gcc
```

Build:

```
cargo build --target=armv7-unknown-linux-gnueabihf --release
```

## Systemd example service

```
[Unit]
Description=Ada client service

[Service]
Restart=always
WorkingDirectory=/home/root/
RestartSec=10
User=root
Environment="HOME=/home/root/"
ExecStart=/opt/ada-client/ada-client

[Install]
WantedBy=multi-user.target
```
