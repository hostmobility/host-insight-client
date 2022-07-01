#!/usr/bin/env bash

# Copyright (c) 2020, 2021 Host Mobility

# Mostly taken from the environmental test for mx5-pt.

# The caller of this script redirects stdout to a test-specific log file and
# stderr to a global fail log file.

function print_usage()
{
  echo "Usage: $0 [OPTION..]"
  echo
  echo "Options:"
  echo "  -h, --help            Print this usage and exit"
  echo "  -d, --disable         Disable modem after exit"
  echo "  -v, --verbose         Print debug info during execution"
  echo "  -r, --reboot          Restart unit and reconnect after factory configuration"
  echo "  -f, --factory=CMDS    Factory default commands required for first-time setup."
  echo "                        Add as many space-separated AT commands as you need, for example"
  printf "                        %s\n" '"AT+QSIMDET=1,0\\r AT+QDAI=5,0,0,4,0,1\\r"'
}

function parse_params()
{
  disable_modem=0
  modem_reboot_flag=0
  factory_default_commands=0
  while :; do
    case "${1-}" in
      -h | --help)
        print_usage
        exit 0
        ;;
      -d | --disable)
        disable_modem=1
        ;;
      -v | --verbose)
        set -x
        ;;
      -r | --reboot)
        modem_reboot_flag=1
        ;;
      -f | --factory)
        factory_default_commands="${2-}"
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

  return 0
}

function modem_setup()
{
  echo "Begin setup"
  # Is the machine MX-V?
  if gpiofind MODEM_ENABLE_ON; then
    gpioset $(gpiofind MODEM_ENABLE_ON)=1
    if (( $? != 0 )); then
      echo "FAILED gpioset (gpiofind MODEM_ENABLE_ON)!"
    fi
  ## If we need to cotroll two sim in this test in th future sim1 is controlled by a 0 and sim2 by 1.
  ##  gpioset $(gpiofind SIM_SEL)=0
  ##  if [ $? != 0 ]; then
	##echo "FAILED gpioset (gpiofind SIM_SEL!" >> $SETUP_LOG
  ##fi
  else
  echo "Assuming MX-4"
  # Assume MX-4
    WAIT_TIME=0
    WAIT_EXIT=1
    echo "check co cpu modem status"
    until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 5 ]; do
      echo 1 > /opt/hm/pic_attributes/ctrl_modem_on
      sleep 6
      RESULT=$(grep -c "Ctrl state:     ON" /opt/hm/modem_status.sh)
      if (( RESULT == 1 )); then
        echo "modem started"
        WAIT_EXIT=0
      fi
      WAIT_TIME=$((WAIT_TIME+1))

    done
  fi

  WAIT_TIME=0
  WAIT_EXIT=1
  echo "Determining modem type..."
  until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 60 ]; do
    if [ -e /dev/ttyACM2 ]; then # This means PLS-8 modem is used
      echo "PLS-8 modem detected"
      MODEM_TYPE="pls8"
      AT_MODEM_DEV_FACTORY=/dev/ttyACM1
      AT_MODEM_DEV=/dev/ttyACM2
      WAIT_EXIT=0
    fi
    if [ -e /dev/ttyUSB2 ]; then # EG25-G modem is used
      echo "EG25-G modem detected"
      MODEM_TYPE="eg25g"
      AT_MODEM_DEV=/dev/ttyUSB2
      AT_MODEM_DEV_FACTORY=/dev/ttyUSB2
      WAIT_EXIT=0
    fi
    WAIT_TIME=$((WAIT_TIME+1))
    sleep 1
  done

  if [ $WAIT_TIME -eq 120 ]; then
    echo "FAILED to setup Modem, try again!"
    return 1
  fi

  return 0
}

function modem_setup_pls8()
{
  WAIT_TIME=0
  WAIT_EXIT=1
  until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 60 ]; do
    sleep 1
    echo -ne "AT^SGPSC=?\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    WAIT_EXIT=$?
    WAIT_TIME=$((WAIT_TIME+1))
  done

  if [  $WAIT_TIME -eq 60 ]; then
    echo "FAILED to setup Modem, try again!"
    return 1
  fi

  WAIT_EXIT=1
  WAIT_TIME=-1
  until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 10 ]; do
    WAIT_TIME=$((WAIT_TIME+1))
    if ! echo -ne "AT^SGPSC=\"Engine\",0\r\n" | microcom -X ${AT_MODEM_DEV} -t 100 | grep "OK"; then
      continue
    fi
    echo -ne "AT^SGPSC=\"Engine\",0\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT^SGPSC=\"Nmea/Freq\",1\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT^SGPSC=\"Nmea/Glonasst\",on\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT^SGPSC=\"Nmea/Output\",on\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT^SGPSC=\"Nmea/Urc\",off\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT^SGPSC=\"Power/Antenna\",auto\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT^SGPSC=\"Engine\",1\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 500

    echo -ne "AT^SGPSC?\r\n" | microcom -X ${AT_MODEM_DEV} -t 100 |    grep "Engine\",\"1"
    WAIT_EXIT=$?
  done

  if [ $WAIT_TIME -eq 10 ]; then
    echo "FAILED to setup Modem, try again!"
    return 1
  fi
  #Check message should look like this:
  #^SGPSC: "Engine","1"
  #^SGPSC: "Info","Urc","off"
  #^SGPSC: "Nmea/Freq",1
  #^SGPSC: "Nmea/Glonass","on"
  #^SGPSC: "Nmea/DeadReckoning","off"
  #^SGPSC: "Nmea/DRSync","off"
  #^SGPSC: "Nmea/Output","on"
  #^SGPSC: "Nmea/Urc","off"
  #^SGPSC: "Power/Antenna","auto"
  return 0
}

function modem_setup_eg25g()
{
  MODEM_TYPE="eg25g"
  WAIT_TIME=0
  WAIT_EXIT=1
  until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 60 ]; do
    sleep 1
    echo -ne "AT+QGPSCFG=?\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    WAIT_EXIT=$?
    WAIT_TIME=$((WAIT_TIME+1))
  done

  if [ $WAIT_TIME -eq 60 ]; then
    echo "FAILED to setup Modem, try again!"
    return 1
  fi

  WAIT_EXIT=1
  WAIT_TIME=0
  until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 10 ]; do
    echo -ne "AT+QGPS=1\r" | microcom -X ${AT_MODEM_DEV} -t 100
    usleep 100
    echo -ne "AT+QGPSCFG=\"nmeasrc\"\r" | microcom -X ${AT_MODEM_DEV} -t 100 > modem_setup.txt
    cat modem_setup.txt
    WAIT_EXIT=$?
    WAIT_TIME=$((WAIT_TIME+1))
  done

  if [ $WAIT_TIME -eq 10 ]; then
    echo "FAILED to setup Modem, try again!"
    return 1
  fi
  return 0
}

# We need some default parameters that are different beetween platforms.
# Some for example have sim detect off and others on with pull up or pull down triggered.
function modem_factory_setup()
{
  MODEM_FACTORY_PATH=/tmp/modem_factory_setup_"$MODEM_TYPE".txt
  WAIT_TIME=0
  WAIT_EXIT=1
  local ret

  #test communication
  until [ $WAIT_EXIT -eq 0 ] || [ $WAIT_TIME -eq 60 ]; do
    sleep 1
    if [[ $MODEM_TYPE = "eg25g" ]]; then
      # TODO: Don't perform factory reset if not necessary
      echo -ne "AT+QGPSCFG=?\r\n" | microcom -X ${AT_MODEM_DEV} -t 100
    elif [[ $MODEM_TYPE = "pls8" ]]; then
      ret=$(echo -ne "AT\r" | microcom -X ${AT_MODEM_DEV_FACTORY} -t 100)
      if [[ $ret =~ .*OK.* ]]; then
        echo "OK response form ${AT_MODEM_DEV_FACTORY}."
        echo "Begin factory setup."
        break
      fi
      ret=$(echo -ne "AT\r" | microcom -X ${AT_MODEM_DEV} -t 100)
      if [[ $ret =~ .*OK.* ]]; then
        echo "OK response from ${AT_MODEM_DEV}."
        echo "Factory setup skipped."
        return 0
      fi
    else
      echo "Factory setup failed: unknown modem"
      return 1
    fi
    WAIT_EXIT=$?
    WAIT_TIME=$((WAIT_TIME+1))
  done

  if [ $WAIT_TIME -eq 60 ]; then
    echo "FAILED to setup factory command Modem, try again!"
    return 1
  fi

  #Send x commands.
  for commands in ${factory_default_commands}; do
    echo -ne "${commands}\r" | microcom -X ${AT_MODEM_DEV_FACTORY} -t 200 > "$MODEM_FACTORY_PATH"
    WAIT_EXIT=$?
    # Read and verify that the command was OK. detect error and microcom errors.
    printf "INFO installed: \"%s\" with return code %d.\n" "${commands}\r" "$(cat $MODEM_FACTORY_PATH)"
    ret=$(grep -c "ERROR" $MODEM_FACTORY_PATH)
    if (( ret == 1 )) || (( WAIT_EXIT != 0 )); then
      echo "FAIL factory settings"
      return $WAIT_EXIT
    fi
  done

  ## Restart modem if this flag is true and try to reconnect before continue. Use both soft reset and hard reset.
  if (( modem_reboot_flag == 1 )); then
    echo -ne "AT+CFUN=1,1\r" | microcom -X ${AT_MODEM_DEV} -t 100
    sleep 1
    if gpiofind MODEM_ENABLE_ON; then
      if gpioset $(gpiofind MODEM_ENABLE_ON)=1; then
        echo "FAILED gpioset (gpiofind MODEM_ENABLE_ON)!"
      fi
    else
      echo 0 > /opt/hm/pic_attributes/ctrl_modem_on
    fi
    sleep 4
    modem_setup
  fi

  return ${WAIT_EXIT}
}

function cleanup()
{
  if (( disable_modem == 1 )); then
    if gpiofind MODEM_ENABLE_ON; then
      if gpioset $(gpiofind MODEM_ENABLE_ON)=1; then
        echo "FAILED gpioset (gpiofind MODEM_ENABLE_ON)!"
      fi
    else
      echo 0 > /opt/hm/pic_attributes/ctrl_modem_on
    fi
  fi
}

function trapper()
{
  cleanup
  exit 1
}

RET=1

trap trapper SIGINT

parse_params "$@"

RET=$?

if (( RET == 0 )); then
  modem_setup
  RET=$?
fi

#factory settings if any. skip if empty(0).
if (( RET == 0 )); then
  if [[ "${factory_default_commands}" != "0" ]]; then
    modem_factory_setup
    RET=$?
  fi
fi

if (( RET == 0 )); then
  if [[ "$MODEM_TYPE" = "pls8" ]]; then
    modem_setup_pls8
    RET=$?
  elif [[ "$MODEM_TYPE" = "eg25g" ]]; then
    modem_setup_eg25g
    RET=$?
  else
    RET=1
  fi
fi

#set failed log.
if (( RET != 0 )); then
  echo "MODEM_SETUP:FAIL" 1>&2
fi

cleanup

exit $RET
