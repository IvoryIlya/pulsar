#!/bin/sh
# Boot hook, started in background by /etc/init.d/S99zpulsar (written by pulsar-install), which rcS
# runs after the firmware's S97quasar / S99quasar (onboot.sh: adbd, quasar in background). Stations
# set up by older installers may also start it from /data/quasar/rootkeeper.sh.
# If /data/pulsar/enabled exists: stop the Yandex stack started by onboot.sh and run
# pulsar under a restart loop. adbd and dropbear are never touched.
D=/data/pulsar
[ -f "$D/enabled" ] || exit 0
# Never stop the Yandex stack when there is nothing to replace it with.
[ -x "$D/pulsar" ] && [ -x "$D/supervise.sh" ] || exit 0
# Once per boot: two hooks (S99zpulsar and a leftover rootkeeper.sh line) may start this at the same
# time, and both would pass the pidof checks below before either supervisor exists. Never removed:
# releasing it before the exec would reopen that window; /tmp is tmpfs, so a reboot clears it.
mkdir /tmp/pulsar-boot.lock 2>/dev/null || exit 0
pidof pulsar >/dev/null 2>&1 && exit 0
pidof supervise.sh >/dev/null 2>&1 && exit 0  # never two supervisors
# Supported models only (see satellite/crates/pulsar-core/src/model.rs); anything else keeps the
# stock stack. While a new port is brought up, /data/pulsar/model (one line: a known codename,
# e.g. "irbisA") takes the place of the devicetree check — the Irbis A reports only the generic
# "amlogic, axg".
if [ -s "$D/model" ]; then
  case "$(head -n1 "$D/model" | tr -d '\r' | tr -d '[:space:]')" in
    cucumber | plum | station-mini2 | irbisA) ;;
    *) exit 0 ;;
  esac
else
  tr '\0' '\n' </proc/device-tree/compatible |
    grep -qxE 'yandex,(cucumber|plum|station-mini2)|irbis,[a-z0-9_-]+' || exit 0
fi

# Stop the stock voice stack before the daemon takes the audio devices. A port's own
# /data/pulsar/stop_oem.sh wins (the Irbis A's firmware has a /system/vendor/quasar of its own,
# but a different launcher and no quasar_launcher2 to wait for). On the Yandex Stations onboot.sh
# has started quasar in background before S99zpulsar runs: wait for it, then stop it.
if [ -x "$D/stop_oem.sh" ]; then
  "$D/stop_oem.sh" >/dev/null 2>&1
elif [ -x /system/vendor/quasar/q ]; then
  i=0
  while [ $i -lt 30 ] && ! pidof quasar_launcher2 >/dev/null 2>&1; do
    sleep 1
    i=$((i + 1))
  done
  /system/vendor/quasar/q stop >/dev/null 2>&1
fi

exec setsid "$D/supervise.sh" </dev/null >/dev/null 2>&1
