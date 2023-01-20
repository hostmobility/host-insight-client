#!/bin/sh

# Exit code handler script for Ada-client

etime=62

if [ "$EXIT_STATUS" = "$etime" ]; then
  logger "Ada-client timed out. Rebooting system"
  systemctl reboot
fi
