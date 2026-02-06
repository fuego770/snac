#!/bin/bash
INTERFACE=$1
ACTION=$2
USER_NAME="yourusername" # <- change your username here
USER_ID=$(id -u $USER_NAME)
DBUS_ADDRESS="unix:path=/run/user/$USER_ID/bus"
CONFIG_FILE="/home/$USER_NAME/.config/sophos_monitor/sophos.config" # need to change the code here for config accordingly

# Load the config file variables directly
if [ -f "$CONFIG_FILE" ]; then
  source "$CONFIG_FILE"
else
  exit 0
fi

notify() {
  sudo -u $USER_NAME DBUS_SESSION_BUS_ADDRESS=$DBUS_ADDRESS notify-send "Sophos" "$1" -i network-wireless
}

case "$ACTION" in
up | vpn-up)
  CUR_SSID=$(nmcli -t -f active,ssid dev wifi | grep '^yes' | cut -d: -f2)
  CUR_CON=$(nmcli -t -f NAME,DEVICE connection show --active | grep "$INTERFACE" | cut -d: -f1)

  if [[ "$CUR_SSID" == "$TARGET_SSID" ]] || [[ "$CUR_CON" == "$TARGET_ETH_NAME" ]]; then
    notify "College Network Detected. Starting Sophos..."
    sudo -u $USER_NAME DBUS_SESSION_BUS_ADDRESS=$DBUS_ADDRESS systemctl --user start sophos-connect.service
  else
    sudo -u $USER_NAME DBUS_SESSION_BUS_ADDRESS=$DBUS_ADDRESS systemctl --user stop sophos-connect.service
  fi
  ;;
down | vpn-down)
  sudo -u $USER_NAME DBUS_SESSION_BUS_ADDRESS=$DBUS_ADDRESS systemctl --user stop sophos-connect.service
  ;;
esac
