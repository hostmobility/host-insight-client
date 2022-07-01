#!/bin/bash

cd ada
rm -f /var/lock/LCK..ttyUSB2

./modem_setup.sh
if (( $? == 0 )); then
  ./gps.sh
  wvdial &
  sleep 2
fi

if [[ -e client-new ]]; then
  chmod +x client-new
  ./client-new
  if (( $? == 0 )); then
    rm client
    mv client-new client
  else
    rm client-new
    ./client
  fi
elif [[ -x client ]]; then
  if [[ -e conf.toml-new ]]; then
    mv conf.toml conf.toml-old
    mv conf.toml-new conf.toml
  fi

  ./client
  if (( $? != 0 )); then
    mv conf.toml-old conf.toml
  fi
fi
