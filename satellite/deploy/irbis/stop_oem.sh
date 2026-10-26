#!/bin/sh
# Stops the stock voice/input stack so pulsar owns the audio devices and the buttons.
# Installed by pulsar-install as a DEFAULT and never overwritten: edit freely on the device.
# Stock stack of firmware 1.51: /system/vendor/quasar/quasar_launcher (started by run.sh from
# S99onboot.sh; the stock stop is `pkill quasar_launcher`) spawns maind --audiod (holds the
# capture device), --yiod, --wifid, --mediad, --linkplay_a98; /usr/bin/btn_listener handles the
# adc_keypad volume/mute keys through its own amixer calls and would double-act on every press
# next to pulsar. linkplay/rootApp and the Bluetooth stack are left running: they do not touch
# the microphone.
killall quasar_launcher 2>/dev/null
sleep 1
killall maind 2>/dev/null
killall btn_listener 2>/dev/null
exit 0
