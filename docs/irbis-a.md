# Irbis A port

Experimental port of pulsar to the **Irbis A** smart speaker (Amlogic Meson A113X, board
identity `A98SmartAudio_*`). Unlike the Yandex Stations, this device carries no usable Yandex
voice stack for pulsar **as far as confirmed so far**: the wake word runs in Home Assistant,
playback goes through a built-in WAV fallback player, the glibc is too old for the daemon
binary (a sidecar runtime is deployed next to it), and the installer talks to the device over
ssh/scp instead of adb. Everything else (the ESPHome Native API server, Sendspin, the mixer,
the supervisor, updates) is the same daemon.

Status: implemented against measured device facts; **not yet run end-to-end on the live
device**. Yandex Station behaviour is unchanged by this port (the golden API tests describe the
same wire bytes).

## Confirmed device facts

| Fact | Value (measured on the device) |
|---|---|
| SoC | Amlogic Meson A113X (AXG family, quad Cortex-A53) |
| Kernel / userland | Linux 4.9.68 **aarch64** (2019-06) with a **32-bit armhf userland**, hard-float loader `/lib/ld-linux-armhf.so.3` |
| **glibc** | **2.23** — older than Rust's gnu-target floor (2.28): the daemon runs through the **glibc sidecar** below |
| Devicetree | `model` = "Amlogic", `compatible` = **"amlogic, axg" only** — nothing device-specific, so model detection needs the override below |
| Storage | `/data` → symlink into `/bkupgrade` (ubi, 46.6 MB total, ~29 MB free — see the space note); rootfs `/` = ubi 40.3 MB |
| Init | busybox `rcS` with `/etc/init.d/S??*` (`S05linkplay.sh`, `S50dropbear`, `S98btn_listener`, `S99onboot.sh`, …) — `S99zpulsar` sorts last ✓ |
| Capture | TDM-C, `hw:0,1` — strictly 8-channel interleaved, 48 kHz, S16_LE (`period 512` / `buffer 2048`); mics on ch 0/1 (ch 1 cleanest), ch 2 = internal acoustic loopback (AEC reference); from the OEM DSP config (`hisf_config.ini`) + measurement |
| Playback | TDM-B, `hw:0,0`, through the firmware's `dmixer_auto`; `/etc/asound.conf` already defines `dmixer`, `dmixer_auto`, `assist_mic`, `assist_dual_mic`, `dmicarray`, `rate_convert`, `asymed`, `softvol` |
| dmix ipc keys | the firmware's dmix uses **0x400/0x401 — the same keys `supervise.sh`'s stale-lock cleanup already removes** |
| Buttons | **two** evdev devices (confirmed via `/proc/bus/input/devices`): `adc_keypad` (event0) = KEY_MUTE (113), KEY_VOLUMEDOWN (114), KEY_VOLUMEUP (115), KEY_POWER (116); `gpio_keypad` (event1) = a single key, code 123 (KEY_WWW) — the action/Alice trigger. The stock `/usr/bin/btn_listener` (S98) reads event0 for its own amixer volume/mute handling |
| LED ring | IS31FL3236/SN3236 on `/dev/i2c-0` addr 0x3c, **8 RGB LEDs** (24 PWM channels): SHUTDOWN 0x00, PWM 0x01+3·i, UPDATE 0x25, per-channel enable 0x27.., FREQ 0x4A (register sequence from the working `manage_led` bring-up tool). **Driven by pulsar**: an i2c sink in `pulsar-hw::leds` (`LedStrip::i2c_dev/i2c_addr`), so the voice indication, volume arc, mute wave and the HA «Ring» light with all its effects work; `/sys/class/leds` is empty (no kernel LED framework) |
| ALSA library | **no system-wide libasound**: `/usr/lib`/`/lib` have none. Two builds exist: `/system/vendor/quasar/libs/libasound.so.2.0.0` and the `override_libs/` one the stock audiod actually runs with (launch.json `ldLibraryPrefix`; same patched-dmix family as the Yandex Stations — it owns the ipc keys 0x400/0x401). The sidecar loader's `--library-path` prefers `override_libs`, then `/lib:/usr/lib`, then `libs/`; the rate plugins and the `/lib/linkplay_a98/alsa-lib` + `/usr/share/alsa` symlinks S99onboot.sh creates serve both |
| Stock stack | **quasar exists**: `/system/vendor/quasar/quasar_launcher launch.json` + `maind --audiod/--yiod/--wifid/--mediad/--linkplay_a98` (a Yandex-OEM variant over the LinkPlay board stack: `rootApp`, `linkplay_bonjour`, `bsa_server` BT), 2020-01 build |
| Voice engine | An **older-generation SDK**, not the Stations' `libyandex_evil`: `libspeechkit.so` *calls* `spotter_create` / `spotter_conf_read` / `spotter_submit_raw_data` / `spotter_get_next_event` (single-channel `spotter_create`, not `spotter_create_multichannel`); models are Kaldi-style (`spotter_model/{alisa,yandex}/acoustic_model.nnet, lda.mat, flags.txt, words.txt`); no library in `libs/` exports the spotter symbols (the implementation is most likely static inside `maind`) |
| Audio front-end | `hisf_config.ini` ("hr-alexa-2.0.5") configures a **userspace** AEC/NS/VAD/AGC/BSS front-end run by `maind --audiod` over `MIC_ORDER={0,1}` with `AEC_REF_CHANS={2}` (per-profile modes: WKP `AEC_MODE=5`, ASR `NS_MODE=3`…). The TDM capture channels themselves are **raw** — echo cancellation exists only inside the stock daemon's chain. The code is `libhrsc.so` = **Horizon Robotics speech core** (`hobot::speech::SpeechEngine`, exported C functions `HisfInit`, `HisfDoVad`, `HisfDoBf`, `HisfAgcFloatProc`, `HisfMcpfProc`, …); `hisf.so` is **not ELF** — it is an encrypted algorithm/data blob (`HISF_READ_DATA_PATH`, the SDK's `hisf_config_encrypt.ini` sibling). The modern public edition of this SDK (Apache-2.0 headers + libs, `hrsc_sdk.h` with `ref_ch_index`/wakeup callbacks) lives at github.com/D-Robotics/hobot_audio — a reference for the family, four years newer than this build |
| Wake-word plumbing | `libspeechkit.so` is a **Yandex SpeechKit (desktop build)** C++ layer (`SpeechKit::PhraseSpotterModelImpl::submitRawData/getNextEvent/recreateSpotter`, TeamCity RPATH) that *calls* the older single-channel spotter C API (`spotter_create`, `spotter_conf_read`, …). No library probed so far **exports** those C symbols — the provider is still unlocated (candidates: an IMPORT resolved from maind's global scope, a library under `/bkupgrade/data/quasar`, or static-and-hidden inside maind) |
| GStreamer | `quasar.cfg` runs real gst pipelines (`souphttpsrc ! decodebin ! … ! alsasink device={deviceName}`) in `maind --mediad`, but neither `quasar/libs` nor `/system/workdir/lib` contains any `libgst*`/`libglib*` (probed) — the GStreamer is linked into `maind` itself and cannot be reused: the WAV fallback player stays the playback backend |
| Stock volume | `volumed`: `volumeStep 1`, `volumeDefault 8` (no Yandex `platformToAlsaMap`) — pulsar keeps its own softvol curve (`volume_map`) |
| Network | `wlan0` present (MAC feeds the mDNS name suffix) |
| SSH | dropbear as a multi-call binary: no SFTP; `scp -O` works; the applet was present but not symlinked into PATH (the installer creates `/usr/bin/scp`) |
| busybox applets | `setsid`, `pidof`, `killall`, `sed`, `awk`, `ipcs`, `wc`, `head` confirmed present |

Because the raw capture device only accepts the 8-channel/48 kHz layout, the daemon never opens
it directly. The installer writes an alsa-lib block into `/root/.asoundrc`
(`satellite/deploy/irbis/asoundrc`, between `# --- pulsar begin/end ---` markers) that defines:

- `pcm.pulsar_mic` — `rate` over `route`: extracts channel 1 of `hw:0,1` with the measured 8×
  digital boost (≈ +18 dB) and resamples to **16 kHz mono** — what the voice pipeline streams to
  Home Assistant. This is `Model::capture_pcm` of the port.
- `pcm.speakers` — `plug` over `softvol` ("Master Playback Volume", min −60 dB) over
  `dmixer_auto`: the playback PCM the mixer, the WAV player and the Sendspin sink already use on
  the Yandex Stations, so those services work unchanged.
The block deliberately does **not** override `pcm.default`: the stock stack's own default chain
creates the `'Master'` softvol control its volume scripts use, and `/root/.asoundrc` is read
again when `ctl.sh disable` gives the device back.

alsa-lib reads `~/.asoundrc` after `/etc/asound.conf`; the daemon runs as root and
`supervise.sh` exports `HOME=/root`, so no firmware file under `/etc` is touched.

## The glibc sidecar (why the binary runs at all)

The firmware's glibc is **2.23**; the Rust toolchain this repository pins (edition 2024 ⇒
≥ 1.85) builds `*-gnu` binaries whose std needs **glibc ≥ 2.28**, and zig cannot link a working
binary below that either. Rather than re-architect the daemon (static musl cannot `dlopen`
libasound, which `pulsar-alsa` is built around), the port ships a **newer armhf glibc runtime
next to the daemon** and starts it through that runtime's loader:

```sh
python3 satellite/deploy/glibc/fetch.py     # once per checkout: Debian bookworm libc6 (2.36,
                                            # the Midi's generation) -> satellite/deploy/glibc/out/
```

`pulsar-install` copies the payload to `/data/pulsar/glibc/` on irbisA targets, and
`supervise.sh` runs the daemon as `glibc/pulsar --library-path …` — where `glibc/pulsar` is a
symlink to the sidecar's `ld-linux-armhf.so.3` (so the kernel's `comm` stays `pulsar` and every
`pidof`/`killall` in the scripts keeps working). The device's `libasound.so.2` and its dlopened
ALSA plugins were built against glibc 2.23 and resolve fine against the newer sidecar libc
(backward symbol versioning); NSS `files`/`dns` are built into glibc 2.36's libc, so name
resolution needs no device files. Kernel 4.9 satisfies the sidecar (bookworm armhf floor: 3.2).

**Space**: `/bkupgrade` has ~29 MB free; the stripped armhf binary + its `.prev` rollback copy +
the ~1.6 MB sidecar + sounds fit, but an update briefly needs a third binary-sized file
(`.new`) — if the volume runs full, drop `pulsar.prev` once a build has proven itself.

## How the port differs from the Yandex Stations

| Area | Yandex Stations (Midi / Lite 2) | Irbis A |
|---|---|---|
| Model detection | `/proc/device-tree/compatible` = `yandex,cucumber` / `yandex,plum` | the devicetree is generic (`amlogic, axg`), so the model comes from **`/data/pulsar/model`** — `pulsar-install --model=irbisA` writes it; `PULSAR_MODEL` overrides for manual runs; an `irbis,<board>` compatible entry would also match |
| Mic front-end | raw TDM/PDM capture → Yandex VQE (`libyandex_evil.so`, dlopen) → Yandex spotters | `pulsar_mic` plug, 16 kHz mono, **no VQE and no spotter** (`Engine::Direct` pass-through in `pulsar-audio`) |
| Wake word | on-device ("Алиса" / "Яндекс"), LAN arbiter between stations | **in Home Assistant**: the daemon keeps a run open (`VoiceAssistantRequest` with the `USE_WAKE_WORD` flag) and streams the mic while idle; HA's pipeline runs its wake-word stage (openWakeWord) on that stream. No arbiter, no local voice commands. Conversations can also be started from HA / the action button (those runs go straight to STT). *If the firmware turns out to carry Yandex's engine libraries (see below), the port can switch to the native path with on-device "Алиса" + VQE* |
| TTS / announcements / earcons | firmware GStreamer (`libquasar_gstreamer.so`), FLAC declared to HA | built-in **WAV fallback player** (`pulsar-media/src/wav.rs`): local files and HTTP RIFF/WAVE S16LE into `speakers`. The media-player entity declares 16 kHz mono WAV for both purposes, and HA transcodes TTS and announcements to exactly that (its `_update_tts_format` / ffmpeg proxy — confirmed in HA source). The GStreamer path is still tried first at runtime |
| Earcon files | `/system/vendor/quasar/sounds/…` | generated WAVs in `satellite/deploy/irbis/sounds/`, installed to `/data/pulsar/sounds/` (`Model::sound_dir`) |
| Music over HTTP (media_player) | any GStreamer codec | WAV only (HA transcodes what it proxies; direct third-party MP3/AAC URLs fail). Music Assistant via **Sendspin works** (its sink writes raw PCM into `speakers`) |
| Buttons | touch keys (`awinic_sar` / `aw9610x`), KEY_VOICECOMMAND, EV_SW mute switch | `keys_input = "adc_keypad,gpio_keypad"` — input.rs now opens **every** comma-separated keys device; KEY_WWW (123) and KEY_PLAYPAUSE (164) act as the action key beside KEY_VOICECOMMAND; KEY_MUTE (113) **toggles** the mic mute (`Model::keys_mute`, no EV_SW switch here; unmuted after boot, shown as the «Hardware mic mute» sensor). KEY_POWER is left unhandled |
| LEDs, clock, light sensor | ring/logo framebuffers, 7-segment clock, LTR308 | the 8-LED ring runs on the raw-i2c sink (above); no clock, no light sensor — those entities are not listed (`UiFeatures`) |
| Zigbee | Midi radio | none |
| Volume | softvol "Master Playback Volume" of the firmware asound config | same control name, created by the installed softvol; the stock curve (`volume_map`) is reused as a plain −60..0 dB in 20 steps |
| Install / update transport | adb (push/pull/shell) | **ssh + `scp -O`** (`ssh://root@<ip>[:port]` targets of `pulsar-install`); key auth expected, the installer symlinks `/usr/bin/scp` → `dropbearmulti` when missing |
| Stock stack handling | `quasar q stop/start`, rootkeeper migration | `/data/pulsar/stop_oem.sh` / `start_oem.sh` — the installer puts **working defaults** there (once, never overwritten): stop kills `quasar_launcher`, `maind` (its `--audiod` holds the capture device) and `btn_listener` (would double-handle every volume/mute press); start relaunches `quasar_launcher` + `btn_listener`. They take precedence over the quasar `q` branch in `boot.sh` (this firmware has a `/system/vendor/quasar` but a different launcher) |
| Autostart | `/etc/init.d/S99zpulsar` after the firmware's quasar script | the same init script; confirmed busybox `rcS` with `S99onboot.sh` present (`S99z` sorts after it); the rootfs guard checks `/etc` against `/` |

## Known limitations (this port stage)

- **No echo cancellation.** Confirmed by `hisf_config.ini`: the AEC (reference = TDM ch 2) runs
  inside the stock `maind --audiod` userspace chain, so raw ALSA capture — what pulsar streams —
  carries the speaker's own output. While music plays loudly, HA-side wake-word detection
  degrades (after detection the dialog ducks music to 15 %, so STT itself is usable). Paths to
  a real fix, in order of effort: software AEC against channel 2 in the front-end (e.g.
  speexdsp); binding the firmware's own `hisf.so` (see the native-engine section).
- WAV-only HTTP playback in the fallback player (no HTTPS, no codecs); Sendspin/MA covers
  serious music playback.
- Wake-word runs stream continuously while idle (by design — that is how HA-side wake word
  works): constant ~256 kbit/s to HA and steady capture CPU.
- Auto-update must stay **off** until a pulsar release actually contains this port (an upstream
  release binary without the `irbisA` model would exit on "unsupported device" and crash-loop
  into rollback).

## The native wake word: what the firmware actually has

The hope was that this quasar variant carries the same engine as the Yandex Stations, so
pulsar could run "Алиса" on-device with VQE echo cancellation. The listings settle it for now:

- **Different SDK generation.** The Stations' engine is `libyandex_evil.so` with
  `YandexVqeC_*` + `spotter_create_multichannel`; here no library exports `YandexVqeC` at all,
  and `libspeechkit.so` only *calls* an older single-channel spotter API (`spotter_create`,
  `spotter_conf_read`, `spotter_submit_raw_data`, `spotter_get_next_event` — the strings found
  are its log messages). The wake models are Kaldi-style directories
  (`spotter_model/{alisa,yandex}/acoustic_model.nnet|lda.mat|flags.txt|words.txt`), not the
  Stations' spotter conf layout.
- **The spotter C API is statically linked and hidden inside `libspeechkit.so`.** Round-2
  probes settled it: the running `maind --audiod`'s `/proc/<pid>/maps` lists every loaded
  library and no spotter provider is among them; `maind`'s dynamic table imports exactly one
  spotter-related symbol — the C++ `SpeechKit::PhraseSpotter::create(Settings const&,
  weak_ptr<PhraseSpotterListener>, shared_ptr<AudioSource>)` from `libspeechkit.so`; and a live
  `dlopen`+`dlsym` of `libspeechkit.so` resolves none of the C `spotter_*` names (its
  `spotter_create() failed` strings are internal log messages around the statically linked
  implementation). The only remaining handle would be the SpeechKit C++ ABI (settings layout,
  listener/source interfaces, `shared_ptr` ABI) — Ghidra-class RE of a proprietary library,
  not worth it against the working HA path. **The native wake word route is closed.**
- **The models confirm the generation**: `spotter_model/alisa/flags.txt` describes a
  `chen14`/`tf-tdnn` Kaldi-style spotter at 16 kHz (`acoustic_model.nnet` + `lda.mat`,
  character-level `words.txt`: filler + а/л/и/с/А), version
  `ru-RU-minispeakers-alisa-02Jul-ads-detector8` — the mini-speaker line, not the Stations'
  multichannel spotter-conf format.
- **AEC lives in the stock daemon, not in the audio path.** `hisf_config.ini` shows the echo
  canceller (ref = TDM ch 2) runs inside `maind --audiod`'s userspace chain
  ("hr-alexa-2.0.5"). `hisf.so` is **not ELF** — an encrypted algorithm/data blob
  (`HISF_READ_DATA_PATH`; the public SDK ships `hisf_config_encrypt.ini` siblings); the code is
  **`libhrsc.so` = Horizon Robotics' speech core** (`hobot::speech::SpeechEngine`). Raw ALSA
  capture — what pulsar and any external tool get — is unprocessed, which is exactly why loud
  playback drowns the talker.

So the shipped wake-word path is **Home Assistant's** (openWakeWord on the streamed audio,
`USE_WAKE_WORD` runs), with the action button and HA-initiated conversations as wake-word-free
alternatives; echo mitigations today: openWakeWord is fairly robust at moderate volume, the
dialog ducks music to 15 % once listening starts, and announcements/TTS/timer rings pause the
wake-word run entirely.

### The open upgrade: binding the hisf front-end (AEC)

What the same probes **opened** is the echo-cancellation route. `libhrsc.so` exports plain-C
entry points the stock audio daemon drives: **`CreateSpeechEngine`**, **`HisfMultichannelAec`**,
`SetNsRefToAecRatio`, `HisfCommonInit{Data,Int}`, `HisfCommonDataMulti`, `HisfCommonDataRead`,
`HisfDoVad`, `HisfDoBf`, `HisfDoaDirection`, `HisfAgcFloatProc`, `HisfMcpfProc`,
`HisfMultiChannelPostFilter`, `HisfGetNoiseVariance`, WebRTC-lineage `Agc*`/`AgcSpl*` helpers,
plus C++ `hobot::speech::SpeechEngine` methods (`hisfupdate`, `destoryEngine`, `getSpeechMode`,
`initSaveFiles`). The modern public edition of this SDK family (Apache-2.0:
github.com/D-Robotics/hobot_audio — `hrsc_sdk.h` with `ref_ch_index`, processed-data callbacks,
wakeup/VAD events) documents the architecture but is a later `hrsc_*` rewrite; the 2020
signatures were never published. Round-3 findings sharpen the target considerably:

- audiod's capture path is `LinkplayA98AudioCapture.cc`, which logs **"Linkplay VQE library
  support: AEC"** and **"Channles: 2"** at startup — so maind consumes the front-end through a
  thin *VQE wrapper* library (prime suspect: `libsoft_prep.so`, never dumped so far). Binding
  that wrapper is potentially far easier than the raw Hisf core.
- libhrsc's strings expose the HISF project layout (`Floating_C/HISF{,_CORE,_NS,_BF,_BSS,_DRB,
  _IVA}/Src/*.c`, WebRTC-lineage `AecProcessBlock`/`AecGetDelay`/`Agc*` internals) and — most
  useful — the core process call's argument names in an error string: **`hisf_out`, `hisf_st`
  (state), `pm_mode`, `tmp_in`**. `pm_mode` selects the ini profile (HISF_WKP / HISF_ASR /
  HISF_COM); note the stock recipes: **WKP runs `AEC_MODE=5`** (wake listening while media
  plays — exactly pulsar's streaming use case), while ASR runs `AEC_MODE=0` (cloud recognition
  happens with playback paused/ducked). A pulsar hisf stage would therefore want the WKP
  profile.
- The startup logs also show the mute path (`AudioEndpoint.cc: Microphones muted/unmuted`) —
  consistent with the momentary KEY_MUTE toggle the port implements.

Round 4 mapped the whole surface:

- **`libhrsc.so` exports 742 plain-C symbols** — the complete HISF toolbox: core
  (`HisfInit`, `HisfProc`, `HisfGetBuffSize`, `HisfIsAecActive`, `HisfDelayEst`,
  `HisfSwitchMode`, `HisfReadConfig`, `HisfDestroy`), modules (`HisfAecInit/HisfAecProc`,
  `HisfMultichannelAec`, `HisfDoVad/DoBf/DoNs/DoAgc/...`, `HisfWkpVad*`), an engine-level C
  API (`CreateSpeechEngine`, `Init`, `StartEngine`, `SyncFeedAudioData`, `SetParams`,
  `StopEngine`, `DestoryEngine`), WebRTC-lineage `Aec*`/`Agc*`/`Bss*`/`Ns*` internals — and a
  licensing surface (`GetLicenseInfo`, wolfSSL, RSA helpers; the device is legitimately
  licensed, so it should pass, but it is a thing to watch).
- **`libsoft_prep.so` is the "Linkplay VQE library"** of the audiod log — and not a function
  library but a **capture server**: `Soft_Prep_Init/GetCapability/GetAudioParam/
  StartCapture/StopCapture/StopWWE`, a unix-socket server ("only support one client"), its own
  ALSA capture, and `_left_/_right_/_ref_process_circ_buffer` rings.
- **audiod runs it AEC-only**: "VQE library support: AEC", "does not support: Wakeup", output
  **16 kHz / 16-bit / 2 channels** — the stock Alice path already uses the front-end exactly
  the way pulsar would.
- **audiod serves "raw audio" on TCP localhost:9899** ("Found audiodRawPort field in common
  config. Starting initServer for raw audio"; control endpoint on 9876). If that stream is the
  VQE output (64 000 B/s for 16 kHz stereo s16), pulsar could consume AEC-processed audio with
  **no DSP binding at all** — at the operational cost of keeping `maind --audiod` alive next
  to pulsar (a selective stop_oem matrix instead of a full stock-stack stop).

That yields three integration strategies, in increasing order of effort:

1. **localhost:9899 consumer** (no RE; probe with `tools/irbis-spotter-probe tcp 9899 8
   out.raw` — the byte rate identifies the format; keep audiod running, kill the rest of the
   stock stack selectively; protocol/handshake TBD from the hex head);
2. **`HisfProc` core binding** (pulsar keeps its own raw capture; `HisfInit` + `HisfProc(st,
   in, out, pm_mode)` with the WKP profile; signatures from disassembling `SyncFeedAudioData`'s
   call into the core — small, paste-able objdump sessions);
3. **engine C API binding** (`CreateSpeechEngine`/`Init`/`StartEngine`/`SyncFeedAudioData` +
   callbacks and the licensing path — the fullest surface, the most unknowns).

The probe tool (`spotter_probe`, workspace artifact — deliberately not in the repository until
something concrete comes of it) covers all three: localhost TCP stream probing with byte-rate
report and raw capture, ELF export dumps, dlopen/dlsym scans, and staged call experiments.

Round 5 (static analysis of the two libraries off-device) recovered the stock chain in full —
the wrapper is `softPrep.h.1.7.0`: `Soft_Prep_Init` → `Soft_prep_hobot_init` →
`hobot_initialize` (handle, six 256 KB ring buffers, `initHobotSdk`, four threads).
`initHobotSdk` drives the hrsc engine C API: `ConfigLogLevel(6)`, `SetParams(0,4,pull_cb)` +
`(1,4,…)`, `(5,4,result_cb)`, `(2,4,5)`, `(10,1,1)`, device id from nvram/`license.txt`,
`Init(id, "/system/workdir/lib")`, `CreateSpeechEngine(tag)`; the capture thread (`recordTest`)
then calls `StartEngine(handle)` and `snd_pcm_open("hw:0,1")`, resamples ch0/ch1/ch2 to 16 kHz
into the rings, and the engine **pulls** through the registered callback, which must deliver
**5-channel interleaved 16 kHz s16 `[L,0,0,R,REF]` in 256-frame (2560 B) blocks**. The mic2ch
callback observed in that path carries the *raw* resampled stereo (1024 B/call) — where (or
whether) the AEC-processed stream leaves the engine in this product configuration is the one
open question (soft_prep registers a **no-op** as the "enhance audio data callback"), together
with whether the licensing (`hobot_auth_cb`, nvram token, wolfSSL) passes outside maind.

Both questions are answered by one on-device experiment (`softprep_probe`, a workspace tool
built from the recovered map: dlopens the four libraries in the stock order, fills the
exported `Soft_prep_hobot` ops table, runs `Soft_Prep_Init` → `setMic2chCallback` →
`Soft_Prep_StartCapture` → `enable(1)`, logs every callback and saves the audio for an A/B
listen against a raw recording while music plays). Depending on its outcome the pulsar
integration is either: consume the processed stream from the wrapper (thinnest), or replicate
the pull-callback recipe above against the hrsc engine directly (everything needed for that is
recovered: the 5-channel input format, the SetParams sequence, the config/resource path), or —
if licensing blocks a foreign process — fall back to a `HisfMultichannelAec`-level binding or
keep the HA-side path as shipped. Sequencing recommendation stands: land and verify the basic
port on the device first; this is an upgrade to the front-end, not a dependency.

## Installing

On the host (Linux):

```sh
make -C satellite ARCH=armhf                 # toolchain per AGENTS.md "Build"
python3 satellite/deploy/glibc/fetch.py      # glibc sidecar payload (needs python3 + network)
ssh-copy-id -i ~/.ssh/id_ed25519 root@<ip>   # once: dropbear reads /root/.ssh/authorized_keys
go -C installer run . --model=irbisA ssh://root@<ip>
```

The installer probes the loader (armhf confirmed) and the device libc, copies the daemon,
scripts, sidecar, asoundrc block (marked; the previous `/root/.asoundrc` is backed up once as
`.asoundrc.pulsar-bak`) and earcons — all checksum-verified (sizes when the busybox lacks
`sha256sum`) — writes `/data/pulsar/model`, `/etc/init.d/S99zpulsar`, and enables the satellite
(health check + `.prev` rollback as on the Stations). Re-running is safe and is the update path.

## Home Assistant setup

1. Add the ESPHome device (`irbis-a-3975dc` style name from the wlan0 MAC; plaintext API until
   HA provisions the Noise key).
2. The satellite has **no device wake words**: the pipeline for it needs a **Wake word** stage
   (openWakeWord). While idle the device keeps a wake-word run open and streams; when HA hears
   the phrase, the same run continues into STT (`WAKE_WORD_END` → `STT_START` in
   `/tmp/pulsar.log`).
3. Conversations can also start from HA (Assist UI / announce with `start_conversation`) or the
   action button (KEY_PLAYPAUSE): those runs skip HA's wake-word stage.
4. TTS/announcements arrive as 16 kHz mono WAV via HA's proxy — make sure HA's (internal) URL
   is reachable over **HTTP** from the speaker; the fallback player does not do HTTPS.

## Bring-up checklist (first live session, remaining items)

1. Sidecar smoke test: `/data/pulsar/glibc/pulsar --library-path /data/pulsar/glibc:/lib:/usr/lib
   /data/pulsar/pulsar --version` — must print the version (proves loader+libc+binary on
   kernel 4.9).
2. `HOME=/root arecord -D pulsar_mic -f S16_LE -r 16000 -c 1 -d 5 /tmp/t.wav` (asoundrc block
   installed) — and `frontend_probe --record` while audio plays: how bad is the echo?
3. `HOME=/root aplay -D speakers /data/pulsar/sounds/spotter_start.wav`; `player_probe`
   (volume ≤ 0.2 per docs/stations.md).
4. ~~buttons~~ done: `adc_keypad` + `gpio_keypad` wired (see the differences table). Verify
   with `hw_probe` after install; KEY_POWER is deliberately unhandled.
5. ~~LEDs~~ done: the IS31FL3236 i2c sink is implemented (8-LED ring); verify with `hw_probe`
   after install — listening/thinking/speaking animations, the volume arc, the mute state.
6. ~~alsa-lib location~~ resolved: the quasar build is the only one and the sidecar's
   `--library-path` carries `/system/vendor/quasar/libs`. Verify the daemon opens `pulsar_mic`
   (`frontend_probe`) and that the quasar libasound honours `/root/.asoundrc` (its alsa.conf
   chain comes from `/usr/share/alsa` → `/system/workdir/lib/alsa`; if the `pulsar_mic`/
   `speakers` definitions are not picked up, `aplay -L` under the daemon's environment will
   show it and the block can move to `/etc/asound.conf` instead).
7. ~~S99onboot.sh~~ captured: `start_oem.sh` now mirrors it (env + `run.sh` + `ntp_sync.sh`),
   `stop_oem.sh` matches the stock `pkill quasar_launcher` + `maind` + `btn_listener`.
8. The remaining native-path probes:
   `strings /system/vendor/quasar/maind | grep -E '^spotter_(create|submit)' | head` (is the
   spotter static in maind?), `cat /system/vendor/quasar/spotter_model/alisa/words.txt
   /system/vendor/quasar/spotter_model/alisa/flags.txt`,
   `strings /system/vendor/quasar/libs/hisf.so | grep -iE '^hisf' | sort -u | head -30`, and
   `ls /system/workdir/lib/ | grep -i 'gst\|glib'` (whether a usable shared GStreamer exists
   for the media player follow-up).

## Repository map of the port

- `satellite/crates/pulsar-core/src/model.rs` — the `IRBIS_A` model, detection + override.
- `satellite/crates/pulsar-audio/src/engine/mod.rs` — `Engine::Direct` (no dlopen, pass-through).
- `satellite/crates/pulsar-media/src/wav.rs` — the WAV fallback player; `player.rs` falls back
  to it when the firmware GStreamer does not load.
- `satellite/crates/pulsar-esphome/` — `MediaFormat` / `UiFeatures`, the `USE_WAKE_WORD` request
  flag, `max_active_wake_words` 0 without device wake words.
- `satellite/crates/pulsar/src/controller/` — `Phase::WakeListen` and the HA wake-word run loop
  (`voice.rs`), model-driven earcons, entity gating.
- `satellite/crates/pulsar-hw/src/input.rs` — several comma-separated keys devices opened at
  once, KEY_PLAYPAUSE/KEY_WWW as action keys, KEY_MUTE toggle (`Model::keys_mute`).
- `satellite/crates/pulsar-hw/src/leds.rs` + `sys.rs` — the raw-i2c sink (IS31FL3236).
- `satellite/deploy/irbis/{stop,start}_oem.sh` — default stock-stack hooks (installed once,
  never overwritten).
- `satellite/deploy/irbis/` — asoundrc block + earcons; `satellite/deploy/glibc/` — the sidecar
  fetcher; `boot.sh` / `ctl.sh` / `supervise.sh` — OEM hooks, model override, loader launch.
- `installer/ssh.go` (+ dispatch in `adb.go`, probes in `probe.go`, device files in `deploy.go`)
  — ssh/scp transport, `--model`, sidecar/asoundrc/earcon deployment.
