//! Per-model hardware description of the supported stations, detected once from the devicetree
//! ("yandex,<codename>" / "irbis,<board>" in /proc/device-tree/compatible). On devices whose
//! compatible string names nothing known (bring-up of a new port; the Irbis A reports only the
//! generic "amlogic, axg"), `PULSAR_MODEL=<codename>` or the first line of `/data/pulsar/model`
//! selects the model instead — logged loudly.

use std::sync::LazyLock;

#[derive(Debug, PartialEq, Eq)]
pub struct Model {
    /// Devicetree codename: "cucumber", "plum", "station-mini2".
    pub codename: &'static str,
    /// ESPHome node name prefix; the MAC suffix is appended.
    pub host_prefix: &'static str,
    /// Shown in Home Assistant / Music Assistant.
    pub friendly_name: &'static str,
    pub model_name: &'static str,
    /// DeviceInfo manufacturer.
    pub manufacturer: &'static str,

    // Capture: interleaved S16 frames from capture_pcm; VQE gets mic_channels / spk_channels.
    // capture_pcm may be an alsa-lib plug (e.g. "pulsar_mic" of the Irbis A asoundrc block) that
    // already extracts and resamples one microphone channel.
    pub capture_pcm: &'static str,
    pub capture_rate: u32,
    pub capture_channels: u32,
    pub mic_channels: &'static [u32],
    pub spk_channels: &'static [u32],
    /// Preset of the firmware's Yandex VQE; "" = the device has no Yandex voice engine: the
    /// capture is used as is (16 kHz mono plug) and the wake word runs in Home Assistant.
    pub vqe_preset: &'static str,

    /// Prefix of the per-channel sysfs LEDs "<led_prefix>:<color>:<ring|logo><index>".
    pub led_prefix: &'static str,
    pub ring: LedStrip,
    /// `logo.count == 0`: no logo.
    pub logo: LedStrip,

    // 4-digit 7-segment clock: "<clock_dir><segment><position>" sysfs LEDs.
    /// e.g. "/sys/class/leds/cucumber:white:clock-"
    pub clock_dir: &'static str,
    /// Name of segment a ('a' or 'A').
    pub clock_seg_base: char,
    /// Name of the leftmost digit position.
    pub clock_pos_base: i32,
    /// Colon LEDs.
    pub clock_colon: &'static [&'static str],
    /// LEDs kept dark.
    pub clock_aux: &'static [&'static str],
    /// LED at g height between digits 1 and 2 ("" = none).
    pub clock_center: &'static str,
    /// true: segments switch 0/255, dimming via the driver chips' "current" attribute;
    /// false: dimming via the per-segment brightness value itself.
    pub clock_chip_current: bool,
    /// false: the station may lack the clock panel; its HA entities then exist only when
    /// `pulsar_clock::panel_present` finds a segment at start, and a missing panel is logged at
    /// info.
    pub has_clock: bool,

    /// evdev device names; `keys_input` is a comma-separated list — every named device is opened
    /// (the Irbis A has its volume/mute keys and its action key on two keypads). `mute_input`
    /// "" = no switch device (the Mini 2's mute is a GPIO, the Irbis A's a KEY_MUTE).
    pub keys_input: &'static str,
    pub mute_input: &'static str,
    /// Mic mute on a sysfs GPIO instead of an evdev switch: global GPIO number, 0 = none; active
    /// low, exported if needed but never configured (used only when already an input).
    pub mute_gpio: u32,
    /// The keys device(s) carry a momentary KEY_MUTE that toggles the microphone mute (no EV_SW
    /// switch and no GPIO line; the Irbis A's adc_keypad). Unmuted after a boot.
    pub keys_mute: bool,
    /// Earcons, relative to `sound_dir` (with its trailing '/'): the firmware's sounds on the
    /// Yandex Stations, the WAVs pulsar installs on ports without them (deploy/<port>/sounds/).
    pub sound_dir: &'static str,
    pub error_sound: &'static str,
    pub wake_sound: &'static str,
    pub cancel_sound: &'static str,
    pub timer_sound: &'static str,
    /// maind plays it (sound 45, SimpleVolumeTonePlayer) for every change except those from its
    /// muted sources "other" (app / remote) and "smart_tts" (smart volume); keys and voice words
    /// click.
    pub volume_sound: &'static str,

    /// ALSA playback PCM (dmix + the softvol "Master Playback Volume"): GStreamer's alsasink,
    /// the Sendspin sink, and the PCM the mixer opens to create the control.
    pub playback_pcm: &'static str,
    /// Stock volume curve: platform level -> raw value (0..255) of the softvol "Master Playback
    /// Volume" on `playback_pcm`, piecewise linear between the points; the first point is
    /// level 0 (raw 0), the last one the top level. quasar.cfg volume_capability
    /// .volume_manager_settings.platformToAlsaMap; the platform level steps by 5 %
    /// (changeStepPercent).
    pub volume_map: &'static [VolumePoint],
    /// true: any audible volume is at least the map's second point (the stock firmware never
    /// sets the raw values below it).
    pub volume_min_step: bool,
    /// false: no ambient light sensor (no Illuminance entity, the sensor is not opened).
    pub has_lux: bool,

    /// Zigbee radio: Telink co-processor on a UART; its reset and boot lines are GPIO value
    /// files (active low, 1 = asserted) exported by the kernel's zb_control driver.
    /// `zigbee_tty` empty: none.
    pub zigbee_tty: &'static str,
    pub zigbee_reset: &'static str,
    pub zigbee_boot: &'static str,
}

impl Model {
    /// The firmware carries the Yandex voice engine (VQE + spotters): the wake word and the
    /// local voice commands run on the device. Without it the front-end streams the capture
    /// plug as is and Home Assistant runs the wake word detection.
    pub fn has_engine(&self) -> bool {
        !self.vqe_preset.is_empty()
    }
}

/// An Nx1 BGR framebuffer behind an ledfb trigger, with per-channel sysfs LEDs as fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedStrip {
    pub count: i32,
    /// sysfs numbering starts at 0 (Midi) or 1 (Lite 2, Mini 2).
    pub first_index: i32,
    /// "/dev/fb0"; "" = none: sysfs LEDs only.
    pub fb_dev: &'static str,
    /// Expected /sys/class/graphics/fbN/name.
    pub fb_name: &'static str,
    /// LED trigger that routes the framebuffer to the LEDs.
    pub trigger: &'static str,
    /// "ring" / "logo"
    pub sysfs_kind: &'static str,
    /// Raw i2c-dev ring (IS31FL3236 family, 8-bit PWM per color): the bus device and the 7-bit
    /// chip address; i2c_dev "" = none. For a ring the kernel exposes neither as a framebuffer
    /// nor as per-LED sysfs files (the Irbis A: /sys/class/leds is empty). Only a ring (no
    /// logo) can be driven this way.
    pub i2c_dev: &'static str,
    pub i2c_addr: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumePoint {
    pub level: i32,
    pub raw: i32,
}

const NO_STRIP: LedStrip = LedStrip {
    count: 0,
    first_index: 0,
    fb_dev: "",
    fb_name: "",
    trigger: "",
    sysfs_kind: "",
    i2c_dev: "",
    i2c_addr: 0,
};

// { "0": 0, "1": 49, "20": 255 }: softvol min_dB -60, so 2.6 dB per level, level 1 at -48.5 dB.
// The Lite 2's quasar.cfg has the same map.
const STOCK_VOLUME: &[VolumePoint] = &[
    VolumePoint { level: 0, raw: 0 },
    VolumePoint { level: 1, raw: 49 },
    VolumePoint {
        level: 20,
        raw: 255,
    },
];

/// Station Midi: aarch64, 4 mics + 4 speaker reference channels at 96 kHz, 24-LED ring + 6-LED
/// logo, clock on two aw21036/aw21012 chips.
static MIDI: Model = Model {
    codename: "cucumber",
    host_prefix: "yandex-midi-",
    friendly_name: "Яндекс Станция Миди",
    model_name: "Yandex Station Midi",
    manufacturer: "Yandex",
    capture_pcm: "hw:0,2",
    capture_rate: 96000,
    capture_channels: 8,
    mic_channels: &[0, 1, 2, 3],
    spk_channels: &[4, 5, 6, 7],
    vqe_preset: "cucumber",
    led_prefix: "cucumber",
    ring: LedStrip {
        count: 24,
        first_index: 0,
        fb_dev: "/dev/fb0",
        fb_name: "ring",
        trigger: "ledfb-ring",
        sysfs_kind: "ring",
        i2c_dev: "",
        i2c_addr: 0,
    },
    logo: LedStrip {
        count: 6,
        first_index: 0,
        fb_dev: "/dev/fb1",
        fb_name: "logo",
        trigger: "ledfb-logo",
        sysfs_kind: "logo",
        i2c_dev: "",
        i2c_addr: 0,
    },
    clock_dir: "/sys/class/leds/cucumber:white:clock-",
    clock_seg_base: 'a',
    clock_pos_base: 1,
    // p3 + q3 form the colon; the rest are auxiliary dots/icons maind never lights for the time.
    clock_colon: &["p3", "q3"],
    clock_aux: &[
        "deg", "h1", "h2", "l1", "l2", "m1", "m2", "p1", "p2", "p4", "p5", "q1", "q2", "q4", "q5",
    ],
    clock_center: "",
    clock_chip_current: true,
    has_clock: true,
    keys_input: "awinic_sar",
    mute_input: "mute",
    mute_gpio: 0,
    keys_mute: false,
    sound_dir: "/system/vendor/quasar/sounds/",
    error_sound: "ru-RU/vins_error.wav",
    wake_sound: "common/spotter_start.mp3",
    cancel_sound: "common/spotter_cancel.mp3",
    timer_sound: "common/timer.wav",
    volume_sound: "common/volume_sound.mp3",
    playback_pcm: "speakers",
    volume_map: STOCK_VOLUME,
    volume_min_step: true,
    has_lux: true,
    // Telink B91 (quasar.cfg zigbee.tty.port). Reset and boot are GPIOZ_11 / GPIOZ_9
    // (devicetree node "zigbee", driver zb_control); maind drives them through these files when
    // quasar.cfg zigbee.zbControl is set, as it is on this firmware.
    zigbee_tty: "/dev/ttyS2",
    zigbee_reset: "/sys/devices/platform/zigbee/zb-reset/value",
    zigbee_boot: "/sys/devices/platform/zigbee/zb-boot/value",
};

/// Station Lite 2: 32-bit userland, 2 PDM mics (capture channels 0 and 2) + speaker feedback
/// (channel 6) at 48 kHz on hw:0,1 (layout of multi_capture_raw in /etc/asound.conf), 3-LED
/// ring, clock LEDs "plum:white:clock<A..H><0..3>" + DH/DL (colon), DM, DEGREE (quasar.cfg
/// /leds/clock).
static LITE2: Model = Model {
    codename: "plum",
    host_prefix: "yandex-lite2-",
    friendly_name: "Яндекс Станция Лайт 2",
    model_name: "Yandex Station Lite 2",
    manufacturer: "Yandex",
    capture_pcm: "hw:0,1",
    capture_rate: 48000,
    capture_channels: 8,
    mic_channels: &[0, 2],
    spk_channels: &[6],
    vqe_preset: "plum",
    led_prefix: "plum",
    ring: LedStrip {
        count: 3,
        first_index: 1,
        fb_dev: "/dev/fb0",
        fb_name: "YNDX LED ring",
        trigger: "led_ring_fb",
        sysfs_kind: "ring",
        i2c_dev: "",
        i2c_addr: 0,
    },
    logo: NO_STRIP,
    clock_dir: "/sys/class/leds/plum:white:clock",
    clock_seg_base: 'A',
    clock_pos_base: 0,
    clock_colon: &["DH", "DL"],
    clock_aux: &["H0", "H1", "H2", "H3", "DEGREE"],
    // Centre dot of the stock voice patterns (their extra LED 35).
    clock_center: "DM",
    clock_chip_current: false,
    has_clock: true,
    keys_input: "aw9610x",
    mute_input: "gpio-keys",
    mute_gpio: 0,
    keys_mute: false,
    sound_dir: "/system/vendor/quasar/sounds/",
    error_sound: "ru-RU/playback_error.mp3",
    wake_sound: "common/spotter_start.mp3",
    cancel_sound: "common/spotter_cancel.mp3",
    timer_sound: "common/timer.wav",
    volume_sound: "common/volume_sound.mp3",
    playback_pcm: "speakers",
    volume_map: STOCK_VOLUME,
    volume_min_step: true,
    has_lux: true,
    zigbee_tty: "",
    zigbee_reset: "",
    zigbee_boot: "",
};

// set_volume.sh on the Mini 2 is `amixer set 'Master' <percent>%`: raw = percent of 255, no dB
// offset and no lowest step.
const MINI2_VOLUME: &[VolumePoint] = &[
    VolumePoint { level: 0, raw: 0 },
    VolumePoint {
        level: 20,
        raw: 255,
    },
];

/// Station Mini 2: Amlogic AXG, 32-bit armhf userland on an aarch64 kernel. Capture is the
/// 6-channel "default" multi device of /etc/asound.conf: 4 PDM mics on channels 0..3 (hw:0,1
/// channels 0/2/4/6) and the 2 TDM amplifier loopback channels on 4/5 (hw:0,0). Playback goes to
/// "default" (softvol "Master" over dmix_spk_plug); there is no "speakers" PCM. The ring is 6 RGB
/// LEDs of an IS31FL3236 without an ledfb trigger (sysfs brightness only). The clock is wired as
/// "mini2:white:clock<A..G><1..4>" + PLUS1/PLUS2 (colon), DEGREE/MINUS (quasar.cfg /leds/clock),
/// but its IS31FL3236 (i2c 0-003f) does not probe on firmware 0.369. No light sensor. Mic mute
/// has no evdev switch: the button is momentary and the mic circuit latches the mute on GPIOZ_3.
static MINI2: Model = Model {
    codename: "station-mini2",
    host_prefix: "yandex-mini2-",
    friendly_name: "Яндекс Станция Мини 2",
    model_name: "Yandex Station Mini 2",
    manufacturer: "Yandex",
    capture_pcm: "default",
    capture_rate: 48000,
    capture_channels: 6,
    mic_channels: &[0, 1, 2, 3],
    spk_channels: &[4, 5],
    vqe_preset: "yandexmini_2",
    led_prefix: "mini2",
    ring: LedStrip {
        count: 6,
        first_index: 1,
        fb_dev: "",
        fb_name: "",
        trigger: "",
        sysfs_kind: "ring",
        i2c_dev: "",
        i2c_addr: 0,
    },
    logo: NO_STRIP,
    clock_dir: "/sys/class/leds/mini2:white:clock",
    clock_seg_base: 'A',
    clock_pos_base: 1,
    clock_colon: &["PLUS1", "PLUS2"],
    clock_aux: &["DEGREE", "MINUS"],
    clock_center: "",
    clock_chip_current: false,
    has_clock: false,
    keys_input: "sar0_pad0",
    mute_input: "",
    // GPIOZ_3: pull-up, held low by the mic circuit while the microphones are cut (maind polls
    // it); GPIOZ_8 only pulses during a press.
    mute_gpio: 414,
    keys_mute: false,
    sound_dir: "/system/vendor/quasar/sounds/",
    // This firmware's GStreamer has no MP3 decoder; every earcon ships as .opus.
    error_sound: "ru-RU/playback_error.opus",
    wake_sound: "common/spotter_start.opus",
    cancel_sound: "common/spotter_cancel.opus",
    timer_sound: "common/timer.opus",
    volume_sound: "common/volume_sound.opus",
    playback_pcm: "default",
    volume_map: MINI2_VOLUME,
    volume_min_step: false,
    has_lux: false,
    zigbee_tty: "",
    zigbee_reset: "",
    zigbee_boot: "",
};

/// Irbis A (Amlogic Meson A113X; board identity "A98SmartAudio", aarch64 kernel 4.9 with a
/// 32-bit armhf userland, glibc 2.23 — the daemon runs through the deployed glibc sidecar).
/// The devicetree is generic ("amlogic, axg" only), so detection goes through
/// `/data/pulsar/model` / `PULSAR_MODEL` (or an "irbis,<board>" compatible entry, should a
/// firmware ever carry one).
///
/// Audio: the OEM DSP delivers the microphone array on the TDM-C capture (`hw:0,1`, strictly
/// 8-channel interleaved S16_LE at 48 kHz, period 512 / buffer 2048), physical microphones on
/// channels 0 and 1 (channel 1 measured cleanest) and the acoustic loopback (the stock AEC
/// reference) on channel 2. The echo canceller itself runs in userspace inside the stock
/// `maind --audiod` (hisf.so), so the raw capture pulsar uses is unprocessed — see
/// docs/irbis-a.md. The `pulsar_mic` plug of the installed asoundrc block
/// (satellite/deploy/irbis/asoundrc) extracts channel 1 with the measured 8x digital boost and
/// resamples to 16 kHz mono. Playback goes through the firmware's `dmixer_auto` dmix on TDM-B
/// (`hw:0,0`), wrapped by the same block into a `speakers` plug (plug -> softvol "Master
/// Playback Volume" -> dmixer_auto) that the mixer, the WAV fallback player and the Sendspin
/// sink expect. No GStreamer is loadable (the firmware's is static inside maind), so the media
/// player runs on the built-in WAV player and declares 16 kHz mono WAV to HA.
///
/// The stock stack is an older Yandex-OEM quasar generation (quasar_launcher + maind, a
/// Kaldi-style spotter_model/, libspeechkit): it does not export the voice-engine API pulsar
/// binds, hence `vqe_preset: ""` — no VQE, no device spotters, the wake word runs in Home
/// Assistant. Input: two GPIO keypads (adc_keypad with KEY_MUTE/VOLUMEDOWN/VOLUMEUP/POWER,
/// gpio_keypad with the single action key KEY_WWW); the KEY_MUTE press toggles the mic mute
/// (`keys_mute`). The ring is 8 RGB LEDs on an IS31FL3236 behind /dev/i2c-0 at 0x3c, driven
/// over raw i2c (no ledfb, /sys/class/leds is empty). No clock, no light sensor, no Zigbee.
static IRBISA: Model = Model {
    codename: "irbisA",
    host_prefix: "irbis-a-",
    friendly_name: "Irbis A",
    model_name: "Irbis A",
    manufacturer: "Irbis",
    capture_pcm: "pulsar_mic",
    capture_rate: 16000,
    capture_channels: 1,
    mic_channels: &[0],
    spk_channels: &[],
    vqe_preset: "",
    led_prefix: "",
    ring: LedStrip {
        count: 8,
        first_index: 0,
        fb_dev: "",
        fb_name: "",
        trigger: "",
        sysfs_kind: "ring",
        i2c_dev: "/dev/i2c-0",
        i2c_addr: 0x3c,
    },
    logo: NO_STRIP,
    clock_dir: "",
    clock_seg_base: 'a',
    clock_pos_base: 0,
    clock_colon: &[],
    clock_aux: &[],
    clock_center: "",
    clock_chip_current: false,
    has_clock: false,
    keys_input: "adc_keypad,gpio_keypad",
    mute_input: "",
    mute_gpio: 0,
    keys_mute: true,
    sound_dir: "/data/pulsar/sounds/",
    error_sound: "error.wav",
    wake_sound: "spotter_start.wav",
    cancel_sound: "spotter_cancel.wav",
    timer_sound: "timer.wav",
    volume_sound: "volume_sound.wav",
    playback_pcm: "speakers",
    // The installed softvol spans -60..0 dB like the Midi's; keep the stock-style lowest step.
    volume_map: STOCK_VOLUME,
    volume_min_step: true,
    has_lux: false,
    zigbee_tty: "",
    zigbee_reset: "",
    zigbee_boot: "",
};

/// Path of the bring-up override file (first line: a codename known to [`model_by_codename`]).
pub const MODEL_FILE: &str = "/data/pulsar/model";

/// The model of this device, read once; None (logged) on unsupported hardware. The devicetree
/// decides; when it names nothing known, `PULSAR_MODEL` or `/data/pulsar/model` may override
/// (bring-up of a port whose compatible string is generic — logged loudly).
pub fn detect_model() -> Option<&'static Model> {
    static MODEL: LazyLock<Option<&'static Model>> = LazyLock::new(|| {
        let raw = std::fs::read("/proc/device-tree/compatible").unwrap_or_default();
        if let Some(model) = model_from_compatible(&raw) {
            return Some(model);
        }
        let shown = String::from_utf8_lossy(&raw).replace('\0', " ");
        if let Some(model) = override_model() {
            log::warn!(
                "devicetree compatible '{shown}' is unknown; using the override model {}",
                model.codename
            );
            return Some(model);
        }
        log::error!("unsupported device (devicetree compatible: {shown})");
        None
    });
    *MODEL
}

/// The model named by `PULSAR_MODEL` or [`MODEL_FILE`], if it names a known codename.
fn override_model() -> Option<&'static Model> {
    let from_env = std::env::var_os("PULSAR_MODEL").map(|v| v.to_string_lossy().into_owned());
    let from_file = std::fs::read(MODEL_FILE).ok().map(|b| {
        String::from_utf8_lossy(b.split(|&c| c == b'\n').next().unwrap_or_default())
            .trim()
            .to_owned()
    });
    let name = match from_env.or(from_file) {
        Some(n) if !n.is_empty() => n,
        _ => return None,
    };
    let model = model_by_codename(&name);
    if model.is_none() {
        log::error!("PULSAR_MODEL / {MODEL_FILE} names the unknown model '{name}'");
    }
    model
}

/// The model with this codename ("cucumber", "plum", "station-mini2", "irbisA").
pub fn model_by_codename(name: &str) -> Option<&'static Model> {
    [&MIDI, &LITE2, &MINI2, &IRBISA]
        .into_iter()
        .find(|m| m.codename == name)
}

/// The model of the first "yandex,cucumber" / "yandex,plum" / "yandex,station-mini2" entry of a
/// NUL-separated compatible list, e.g. "yandex,cucumber-dvt2\0yandex,cucumber\0amlogic, a5\0".
/// Any "irbis,<board>" entry selects the Irbis A: its real firmware reports only the generic
/// "amlogic, axg" (hence the override file), and no second Irbis board is known to distinguish.
pub fn model_from_compatible(raw: &[u8]) -> Option<&'static Model> {
    raw.split(|&b| b == 0).find_map(|entry| match entry {
        b"yandex,cucumber" => Some(&MIDI),
        b"yandex,plum" => Some(&LITE2),
        b"yandex,station-mini2" => Some(&MINI2),
        e if e.starts_with(b"irbis,") && e.len() > b"irbis,".len() => Some(&IRBISA),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cucumber_is_midi() {
        let m =
            model_from_compatible(b"yandex,cucumber-dvt2\0yandex,cucumber\0amlogic, a5\0").unwrap();
        assert_eq!(m.codename, "cucumber");
        assert_eq!(m.capture_rate, 96000);
        assert_eq!(m.logo.count, 6);
        assert_eq!(m.zigbee_tty, "/dev/ttyS2");
    }

    #[test]
    fn plum_is_lite2() {
        let m = model_from_compatible(b"yandex,plum\0amlogic, axg\0").unwrap();
        assert_eq!(m.codename, "plum");
        assert_eq!(m.ring.first_index, 1);
        assert_eq!(m.logo.count, 0);
        assert_eq!(m.zigbee_tty, "");
        // No trailing NUL: the last entry still counts.
        assert_eq!(
            model_from_compatible(b"amlogic, axg\0yandex,plum")
                .unwrap()
                .codename,
            "plum"
        );
    }

    #[test]
    fn station_mini2_is_mini2() {
        let m = model_from_compatible(b"yandex,station-mini2\0amlogic, axg\0").unwrap();
        assert_eq!(m.codename, "station-mini2");
        assert_eq!(m.playback_pcm, "default");
        assert_eq!(m.mute_input, "");
        assert_ne!(m.mute_gpio, 0);
        assert!(!m.has_clock && !m.has_lux && !m.volume_min_step);
    }

    #[test]
    fn irbis_a_matches_the_vendor_prefix_and_the_override_name() {
        for raw in [
            b"irbis,a\0amlogic,a113x\0".as_slice(),
            b"irbis,irbis-a\0".as_slice(),
            b"amlogic, axg\0irbis,a\0".as_slice(),
        ] {
            let m = model_from_compatible(raw).unwrap();
            assert_eq!(m.codename, "irbisA");
        }
        // A bare vendor prefix without a board is not a match.
        assert!(model_from_compatible(b"irbis,\0").is_none());
        assert!(model_from_compatible(b"xirbis,a\0").is_none());
        let m = model_by_codename("irbisA").unwrap();
        // No Yandex engine: 16 kHz mono capture plug, wake word in Home Assistant.
        assert!(!m.has_engine());
        assert_eq!(m.capture_pcm, "pulsar_mic");
        assert_eq!(m.capture_rate, 16000);
        assert_eq!(m.capture_channels, 1);
        assert_eq!(m.mic_channels, &[0]);
        assert!(m.spk_channels.is_empty());
        assert_eq!(
            (m.ring.count, m.ring.i2c_dev, m.ring.i2c_addr),
            (8, "/dev/i2c-0", 0x3c)
        );
        assert_eq!(m.logo.count, 0);
        assert!(!m.has_clock && !m.has_lux);
        assert_eq!(m.keys_input, "adc_keypad,gpio_keypad");
        assert!(m.keys_mute);
        assert_eq!(m.mute_input, "");
        assert_eq!(m.mute_gpio, 0);
        assert_eq!(m.playback_pcm, "speakers");
        assert_eq!(
            format!("{}{}", m.sound_dir, m.wake_sound),
            "/data/pulsar/sounds/spotter_start.wav"
        );
    }

    #[test]
    fn unknown_hardware() {
        assert!(model_from_compatible(b"").is_none());
        assert!(model_from_compatible(b"yandex,cucumber-dvt2\0amlogic, a5\0").is_none());
        assert!(model_from_compatible(b"yandex,yandexmini\0").is_none());
        assert!(model_from_compatible(b"xyandex,plum\0").is_none());
        // The old installer pattern cut "yandex,station-mini2" at the hyphen.
        assert!(model_from_compatible(b"yandex,station\0").is_none());
        assert!(model_by_codename("nope").is_none());
    }

    #[test]
    fn first_known_entry_wins() {
        assert_eq!(
            model_from_compatible(b"yandex,plum\0yandex,cucumber\0")
                .unwrap()
                .codename,
            "plum"
        );
        assert_eq!(
            model_from_compatible(b"yandex,cucumber\0yandex,plum\0")
                .unwrap()
                .codename,
            "cucumber"
        );
    }
}
