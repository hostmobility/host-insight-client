#!/bin/sh

# Exit code handler script for Ada-client

etime=62

exit_code=$1

if [ "$exit_code" -eq $etime ]; then
  systemctl reboot
fi
