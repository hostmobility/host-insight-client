#!/usr/bin/env bash

# Copyright (c) 2020 Host Mobility

# Author: Albin Söderqvist @ Endian Technologies

# A simple GPS test that checks whether a complete GPGGA data entry can be
# received. A more thorough test can be found in
# hm-tester/Application/hm-tester/hm-tester/Tests/Connectivity/GPS.cs

# The caller of this script redirects stdout to a test-specific log file and
# stderr to a global fail log file.

# Led option is MX4 only for now.
# TODO: enable for MX5 using leds api.

SET_LED_FLASH=/opt/hm/set_led_flash.sh

function print_usage()
{
  echo "Usage: $0 [OPTION...]"
  echo
  echo "Options:"
  echo "  -h, --help             Print this usage and exit"
  echo "  -l, --led [N]          Use LED to indicate test result (MX4 only)."
  echo "                         If LED number N (0-9) is provided, then the"
  echo "                         device-specific default can be overridden"
  echo "  -d, --device=PATH      GPS device, e.g. /dev/ttyACM1"
  echo "  -r, --relaxed          Accept only partial GPS data"
  echo "  -t, --timeout=SECONDS  Timeout in seconds"
  echo "  -v, --verbose          Print debug info during execution"
}

function parse_params()
{
  relaxed=false
  use_led=false

  while :; do
    case "${1-}" in
      -h | --help)
        print_usage
        exit 0
        ;;
      -d | --device)
        GPS_DEV="${2-}"
        shift
        ;;
      -l | --led)
        use_led=true
        if [[ ${2:0:1} == "-" || ${2:0:1} == "" ]]; then
          LED=3 # Default GPS value for T30 FR
        else
          LED="${2-}"
          shift
        fi
        ;;
      -r | --relaxed)
        relaxed=true
        ;;
      -t | --timeout)
        TIMEOUT="${2-}"
        shift
        ;;
      -v | --verbose)
        set -x
        ;;
      -?*)
        echo "Unknown option: $1"
        return 1
        ;;
      *)
        break
        ;;
    esac
    shift
  done

  # Validate input
  if [[ -z ${GPS_DEV+x} ]]; then
    if [[ -e /dev/ttyACM1 ]]; then
      #echo "PLS-8 detected"
      GPS_DEV=/dev/ttyACM1
    elif [[ -e /dev/ttyUSB1 ]]; then
      #echo "EG25-G detected"
      GPS_DEV=/dev/ttyUSB1
    else
      echo "Unknown GPS device"
    fi
  fi

  if [[ ! -b $GPS_DEV && ! -c $GPS_DEV ]]; then
    echo "$GPS_DEV is either off or not a device"
    return 1
  fi

  if [[ -n $LED ]] && ! [[ $LED =~ ^[0-9]$ ]]; then
    echo "LED number must be a single digit"
    return 1
  fi

  return 0
}

function set_led_status
{
  if [[ $1 = "working" ]]; then
    # Rapidly blink green to indicate running test
    $SET_LED_FLASH $LED 1 2
  elif [[ $1 = "fail" ]]; then
    # Slowly blink orange/off to indicate error
    $SET_LED_FLASH $LED 2 10
  elif [[ $1 = "ok" ]]; then
    # Enable solid green to indicate success
    $SET_LED_FLASH $LED 1 0
  else
    echo "Invalid led status selected"
  fi
}

function gps_test()
{
  GPS_LOG=/tmp/gps.log

  #if $relaxed; then
    #echo "Begin GPS test (relaxed mode)"
  #else
    #echo "Begin GPS test (strict mode)"
  #fi
  if $use_led; then
    set_led_status "working"
  fi

  if [[ -z ${TIMEOUT} ]]; then
    TIMEOUT=30
  fi

  # Get GPS data
  gps_data=""
  cat "$GPS_DEV" > "$GPS_LOG" &
  cat_pid=$!
  trap trapper SIGINT
  for ((i=0; i <TIMEOUT; i++)); do
    # Filter out GPS data with missing entries
    gps_data=$(grep -v ',,,' "$GPS_LOG" | grep -m 1 GPGGA)

    if [[ -n "$gps_data" ]]; then
      # Print the first complete GPS entry
      #echo "Received complete GPGGA data:"
      echo "$gps_data"
      LAT1="$(echo $gps_data | awk -F "," '{ print $3 }' | sed 's/^0*//')"
      LON1="$(echo $gps_data | awk -F "," '{ print $5 }' | sed 's/^0*//')"
      echo $LON1
      echo $LAT1
      LON=$(echo "$LON1 / 100" | bc -l )
      LAT=$(echo "$LAT1 / 100" | bc -l )
      sed -i 's/longitude.*/longitude = '${LON:0:11}'/g' conf.toml
      sed -i 's/latitude.*/latitude = '${LAT:0:11}'/g' conf.toml
      #echo "($LON, $LAT)" > /tmp/gps_position 
      #cat conf.toml 
      return 0
    fi
    sleep 1
  done

  #print last 30 in list for debug
  gps_partial_data=$(grep -m 30 GPGGA "$GPS_LOG")
  if [[ -n $gps_partial_data ]]; then
    echo "Received only partial GPGGA data:"
    echo "$gps_partial_data"
    if (( relaxed == 1 )); then
      return 0
    fi
  fi

  echo "Failed to receive any GPGGA data"

  return 1
}

function cleanup()
{
  if [[ -z $(kill -0 $cat_pid) ]]; then
    kill $cat_pid
  fi
}

function trapper()
{
  cleanup
  exit 1
}


parse_params "$@"

RET=$?

if (( RET == 0 )); then
  gps_test
  RET=$?
fi

if (( RET == 0 )); then
  if $use_led; then
    set_led_status "ok"
  fi
else
  if $use_led; then
    set_led_status "fail"
  fi
  echo "GPS:FAIL" 1>&2
fi

cleanup

exit $RET
