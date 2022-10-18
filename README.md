# Client

This application implements a grpc client with interrupt-driven async
monitors for reading events on digital-in and/or CAN frames on a
GNU/Linux system and then transmitting them securely (TLS) to a grpc
server. CAN frames are decoded using a DBC file and sent in
a human-readable format.

In addition, the following data is sent:

- GPS location (at boot)
- heartbeat containing the client ID (at some regular interval)

The following responses can be handled:

- Carry on: continue listening for new data
- Software update: download a new version of the client from a predefined location
- Config update: download a new configuration file for the client
- Exit: terminate the application with exit code success

## CAN

The following signal values are supported:

- signed integers
- unsigned integers
- floats including from extended value type list
- strings (enums) from value descriptions

CAN timestamps are not yet implemented.

## Digital in

Digital inputs are fetched from pre-defined line numbers on a
gpiochip, e.g. [5, 6, 7, 8, 9, 10]. The signal source is defined as
"Digital {line number - offset}". If the offset is 5, then the digital
inputs become identified as [0, 1, 2, 3, 4, 5]. The value is a bool.

## Example configuration

The application will look for a conf.toml file firstly in
~/.config/ada-client/ and secondly in /etc/opt/ada-client/.

Example configuration that enables six digital-in and two CAN ports:

```
uid = 42

[gpio]
chip = "/dev/gpiochip10"
lines = [5, 6, 7, 8, 9, 10] # Digital in 0--5 on MX-V PT
offset = 5 # Send values as lines numbers minus the offset, 5 - 5 = 0, 6 - 5 = 1, etc.

[can]
ports = [ { name = "can0", bitrate = 125000, read_only = true  },
          { name = "can1", bitrate = 500000, read_only = false } ]
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

TODO: add required packages

Run the following commands:

```
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=/usr/bin/arm-linux-gnueabihf-gcc

cargo build --release --target=armv7-unknown-linux-gnueabihf
```
