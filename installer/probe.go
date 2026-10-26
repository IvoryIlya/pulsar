package main

import (
	"fmt"
	"slices"
	"strings"
)

// Devicetree codename -> name. Only models listed in abis get installed: the daemon drives
// model-specific hardware (mic array + VQE preset, LEDs, clock, inputs).
func modelName(code string) string {
	switch code {
	case "cucumber":
		return "Yandex Station Midi"
	case "plum":
		return "Yandex Station Lite 2"
	case "station-mini2":
		return "Yandex Station Mini 2"
	case "irbisA":
		return "Irbis A"
	}
	return ""
}

// abis lists the userspace ABIs a model's firmware runs, its usual one first; nil = model not
// supported. The installed binary is build/<arch>/pulsar for the first ABI whose dynamic loader
// is on the station. A second ABI is only for a userspace actually seen on that model: some Midi
// images run a 32-bit userspace on the 64-bit kernel (#13) and need the armhf build (its ALSA
// ABI is the one in audio/alsa_abi.h, #14). Lite 2 and Mini 2 stay armhf-only: no 64-bit
// userspace has been seen on them, and the aarch64 binary needs glibc 2.34 — on a miss it would
// crash-loop back to the Yandex stack (no .prev on a first install).
func abis(code string) []string {
	switch code {
	case "cucumber":
		return []string{"aarch64", "armhf"}
	case "plum", "station-mini2":
		return []string{"armhf"}
	case "irbisA":
		// Seen on the device: a 64-bit kernel (4.9, aarch64) with a 32-bit armhf userland
		// (glibc 2.23 — the daemon runs through the deployed glibc sidecar; the armhf build's
		// own floor is the Makefile's ARMHF_GLIBC, and the probe prints the device libc).
		return []string{"armhf"}
	}
	return nil
}

// archOf picks the model's first ABI whose loader is on the station (loaders: the words the
// loader probe printed). "" = none of the model's ABIs: skip the station.
func archOf(code string, loaders []string) string {
	for _, a := range abis(code) {
		if slices.Contains(loaders, a) {
			return a
		}
	}
	return ""
}

// hasZigbee: the model has the Zigbee radio the daemon can run as a router (radio/), so the router
// firmware image is installed next to the daemon.
func hasZigbee(code string) bool { return code == "cucumber" }

type station struct {
	serial string
	// Devicetree codename (modelName).
	code   string
	arch   string
	zigbee bool
	// The codename came from --model or /data/pulsar/model, not the devicetree: the install
	// writes /data/pulsar/model so the daemon and boot.sh find the model too.
	modelOverride bool
}

// found: installable stations, in the order printed by probe.
var found []station

// stationCode picks the model from the devicetree compatible list (NUL-separated, one entry per
// line or space-separated here). The first entry with a supported codename wins: the list starts
// with board-specific variants, e.g. "yandex,cucumber-dvt2" before "yandex,cucumber" on the Midi.
// The first codename is returned even when nothing is supported, so that probe can name the
// unknown device.
func stationCode(compat string) (string, bool) {
	var first string
	for _, entry := range strings.Fields(compat) {
		if board, ok := strings.CutPrefix(entry, "irbis,"); ok && board != "" {
			// Any Irbis board maps to the one ported model until a second one exists.
			return "irbisA", true
		}
		code, ok := strings.CutPrefix(entry, "yandex,")
		if !ok || code == "" {
			continue
		}
		if first == "" {
			first = code
		}
		if abis(code) != nil {
			return code, true
		}
	}
	return first, false
}

// probe prints one status line for s and records it in found if it is installable. A device we
// connected to ourselves is disconnected again when it is not.
func probe(s string, ours bool) {
	skip := func(msg string) {
		fmt.Printf("  %s: %s\n", s, msg)
		if ours {
			disconnect(s)
		}
	}
	compat, err := sh(s, `tr '\0' '\n' </proc/device-tree/compatible`)
	if err != nil {
		if isSSH(s) {
			skip("ssh not usable (host down, or no key/root login)")
		} else {
			skip("adb not usable (unauthorized or offline)")
		}
		return
	}
	code, supported := stationCode(compat)
	viaOverride := false
	if code == "" || !supported {
		// A generic devicetree (the Irbis A reports only "amlogic, axg"): --model decides,
		// else the model file an earlier install wrote.
		if opts.model != "" {
			code, supported, viaOverride = opts.model, true, true
		} else if m, err := sh(s, "head -n1 "+dataDir+"/model 2>/dev/null"); err == nil {
			if m = strings.TrimSpace(m); modelName(m) != "" {
				code, supported, viaOverride = m, true, true
			}
		}
	}
	if code == "" {
		skip("not a supported station (a port with a generic devicetree needs --model=<codename>)")
		return
	}
	name := modelName(code)
	if name == "" {
		name = "unknown Yandex device"
	}
	name += " (" + code + ")"
	if !supported {
		skip(name + " — not supported yet, skipped")
		return
	}
	if uid, _ := sh(s, "id -u"); uid != "0" {
		skip(name + " — the shell is not root, skipped")
		return
	}
	// `; true`: a missing loader is not an error. Without it the last [ -x ] exits 1 on a
	// normal Midi (no armhf loader) and sh() reports a failure that is not an adb failure.
	loaders, err := sh(s, `[ -x /lib/ld-linux-aarch64.so.1 ] && echo aarch64; [ -x /lib/ld-linux-armhf.so.3 ] && echo armhf; true`)
	if err != nil {
		skip("the device shell is not usable")
		return
	}
	arch := archOf(code, strings.Fields(loaders))
	if arch == "" {
		skip(name + " — unsupported userland ABI, skipped")
		return
	}
	var state string
	if _, err := sh(s, "pidof pulsar >/dev/null"); err == nil {
		state = "satellite running"
	} else if _, err := sh(s, "[ -x "+dataDir+"/pulsar ]"); err == nil {
		state = "satellite installed, not running"
	} else if code == "irbisA" {
		state = "stock firmware"
	} else {
		state = "stock Yandex"
	}
	found = append(found, station{s, code, arch, hasZigbee(code), viaOverride})
	note := ""
	if arch != abis(code)[0] {
		note = ", " + arch + " build"
	}
	if viaOverride {
		note += ", model via override"
	}
	if code == "irbisA" {
		// The armhf build's glibc floor (satellite/Makefile ARMHF_GLIBC) matters on this
		// device: the daemon runs through the deployed sidecar, but the sidecar's loader
		// still needs the kernel's floor kept, and the printout catches a wrong sidecar.
		if g, err := sh(s, `/lib/libc.so.6 2>/dev/null | head -1 || ldd --version 2>/dev/null | head -1 || true`); err == nil && strings.TrimSpace(g) != "" {
			note += "; device libc: " + strings.TrimSpace(g)
		}
	}
	fmt.Printf("  [%d] %s: %s — %s%s\n", len(found), s, name, state, note)
}
