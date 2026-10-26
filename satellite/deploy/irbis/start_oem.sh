#!/bin/sh
# Starts the stock stack again (ctl.sh disable / the supervisor's give-back), mirroring what
# /etc/init.d/S99onboot.sh does for the quasar part: its environment, run.sh (which starts
# quasar_launcher -> maind --audiod/--yiod/--wifid/--mediad/--linkplay_a98) and ntp_sync.sh.
# Installed once as a default and never overwritten by the installer; edit freely.
export LD_LIBRARY_PATH=/system/vendor/quasar:/system/vendor/quasar/libs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
export PATH=/system/vendor/quasar:$PATH
export GST_PLUGIN_PATH=/system/vendor/quasar/libs
[ -x /usr/bin/btn_listener ] && /usr/bin/btn_listener >/dev/null 2>&1 &
if ! pidof quasar_launcher >/dev/null 2>&1; then
  cd /system/vendor/quasar || exit 1
  if [ -x ./run.sh ]; then
    ./run.sh >/dev/null 2>&1 &
  elif [ -x ./quasar_launcher ]; then
    ./quasar_launcher /system/vendor/quasar/launch.json >/dev/null 2>&1 &
  fi
  [ -x ./ntp_sync.sh ] && ./ntp_sync.sh >/dev/null 2>&1 &
fi
exit 0
