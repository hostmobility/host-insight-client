# Configuration for unit number 14

## Introduction

This configuration covers the setup for an experimental/test unit
running client version b457d1d5eafb28449c61dcb377a2632cf41e2284 on an
MX-V PT running Yocto Dunfell.

It starts by setting up the modem, fetching the GPS position and
connecting to the cellular network (no ethernet or wifi required).

The client begins by reading digital input 0--5 and transmitting the
results (true or false) along with the GPS positions. It then
transmits digital values when a state change is detected. In addition,
it transmits a heartbeat signal to the server to show that it's alive.

## Installation instructions

Copy the files to the following locations:

/home/root/ada-launch.sh
/home/root/ada/conf.toml
/home/root/ada/gps.sh
/home/root/ada/modem_setup.sh
/lib/systemd/system/ada.service

Install curl:

```
opkg install curl
```

Make sure that `wvdial` is present and properly configured for mobile
data communication with the mobile network operator.


Enable the systemd service which automatically executes ada-launch.sh

```
systemctl enable ada.service
```

## Physical connections

Install a working SIM card, connect antennas to the GNSS and DIV
ports and (at least) digital in 0--5. Then apply power.
