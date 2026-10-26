package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"
)

// build builds missing outputs in a repository checkout; the release archive ships them and no
// sources.
func build() bool {
	sat := filepath.Join(root, "satellite")
	if _, err := os.Stat(filepath.Join(sat, "Makefile")); err != nil {
		return true
	}
	fmt.Println("== build")
	fail := func(err error) bool {
		fmt.Fprintf(os.Stderr, "build: %v (building from source needs Linux or WSL, see README)\n", err)
		return false
	}
	mk := exec.Command("make", "-C", sat, fmt.Sprintf("-j%d", runtime.NumCPU()))
	mk.Stderr = os.Stderr
	if err := mk.Run(); err != nil {
		return fail(err)
	}
	if opts.ssh {
		for _, a := range []string{"aarch64", "armhf"} {
			if st, err := os.Stat(filepath.Join(root, "ssh", "out", a, "dropbearmulti")); err != nil || st.Size() == 0 {
				b := exec.Command(filepath.Join(root, "ssh", "build.sh"))
				b.Stderr = os.Stderr
				if err := b.Run(); err != nil {
					return fail(err)
				}
				break
			}
		}
	}
	return true
}

// devHasSha256 reports (once per target) whether the device has sha256sum; the Irbis A busybox
// may not, and station.sh-style verification then falls back to file sizes (weaker, but the
// ssh transport already guarantees an intact copy better than the adb pty did).
var devHasSha sync.Map

func devHasSha256(s string) bool {
	if v, ok := devHasSha.Load(s); ok {
		return v.(bool)
	}
	_, err := sh(s, "command -v sha256sum >/dev/null 2>&1")
	has := err == nil
	devHasSha.Store(s, has)
	return has
}

// devSum is the device's checksum of a remote path, comparable with localSumFor: "sha256:<hex>"
// or "size:<bytes>"; "" when the file does not exist (or the read failed).
func devSum(s, remote string) string {
	if devHasSha256(s) {
		out, err := sh(s, "[ -f "+remote+" ] && sha256sum "+remote)
		if err != nil || firstField(out) == "" {
			return ""
		}
		return "sha256:" + firstField(out)
	}
	out, err := sh(s, "[ -f "+remote+" ] && wc -c < "+remote)
	if err != nil {
		return ""
	}
	n := strings.TrimSpace(out)
	if n == "" || strings.ContainsAny(n, "\n ") {
		return ""
	}
	return "size:" + n
}

// localSumFor is the local counterpart of devSum (same method the device supports).
func localSumFor(s, local string) string {
	if devHasSha256(s) {
		if h := sha256File(local); h != "" {
			return "sha256:" + h
		}
		return ""
	}
	fi, err := os.Stat(local)
	if err != nil {
		return ""
	}
	return fmt.Sprintf("size:%d", fi.Size())
}

func sha256File(p string) string {
	f, err := os.Open(p)
	if err != nil {
		return ""
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return ""
	}
	return hex.EncodeToString(h.Sum(nil))
}

// installOne deploys, (re)starts and checks one station; every step checks its own status. note is
// set when autostart could not be (re)written; lost: the station has no autostart at all.
func installOne(st station) (ok bool, note string, lost bool) {
	s := st.serial
	bin := filepath.Join(root, "satellite", "build", st.arch, "pulsar")
	if fi, err := os.Stat(bin); err != nil || fi.Size() == 0 {
		fmt.Fprintf(os.Stderr, "%s: %s missing\n", s, bin)
		return false, "", false
	}
	ts := time.Now().Format("20060102150405")
	changed, supChanged, ok := deployFiles(s, bin, st.arch, st.zigbee, st.code, st.modelOverride)
	if !ok {
		return false, "", false
	}
	fmt.Printf("== %s: autostart\n", s)
	note, lost, ok = bootAutostart(s, ts, st.code)
	if !ok {
		return false, "", false
	}
	if opts.noEnable {
		return true, note, lost
	}
	fmt.Printf("== %s: start\n", s)
	// Marker in the log: the health check looks only at what happens after it.
	mark := fmt.Sprintf("=== install-marker %d %d", os.Getpid(), time.Now().Unix())
	_ = shTo(os.Stdout, s, "echo '"+mark+"' >> /tmp/pulsar.log")
	action := "restart"
	if supChanged {
		action = "reload"
	}
	_ = shTo(os.Stdout, s, "if [ -f "+dataDir+"/enabled ] && { pidof supervise.sh || pidof pulsar; } >/dev/null; then "+
		dataDir+"/ctl.sh "+action+"; else "+dataDir+"/ctl.sh enable; fi")
	fmt.Println()
	if !healthCheck(s, changed, mark) {
		return false, note, lost
	}
	_ = shTo(os.Stdout, s, dataDir+"/ctl.sh status; "+dataDir+"/ctl.sh log 5")
	fmt.Println()
	return true, note, lost
}

// deployFiles copies the daemon, scripts, the radio's firmware images (zigbee) and dropbear (--ssh).
// changed: the binary was replaced; supChanged: supervise.sh differs from the installed one.
func deployFiles(s, bin, arch string, zigbee bool, code string, modelOverride bool) (changed, supChanged, ok bool) {
	changed, supChanged = true, true
	sshOut := filepath.Join(root, "ssh", "out", arch)
	deploy := filepath.Join(root, "satellite", "deploy")
	fmt.Printf("== %s: daemon (%s)\n", s, arch)
	if shTo(os.Stdout, s, "mkdir -p "+dataDir) != nil {
		return
	}
	if modelOverride {
		// The devicetree names nothing known: the daemon (detect_model) and boot.sh read the
		// codename from this file instead.
		if have, _ := sh(s, "head -n1 "+dataDir+"/model 2>/dev/null"); strings.TrimSpace(have) != code {
			if shTo(os.Stdout, s, "printf '%s\\n' "+code+" > "+dataDir+"/model") != nil {
				return
			}
			fmt.Printf("wrote %s/model = %s (generic devicetree)\n", dataDir, code)
		}
	}
	want := localSumFor(s, bin)
	if want == "" {
		fmt.Fprintf(os.Stderr, "%s: cannot checksum %s locally\n", s, bin)
		return
	}
	if want == devSum(s, dataDir+"/pulsar") {
		fmt.Println("binary identical to the installed one, not copied (.prev kept)")
		changed = false
	} else {
		if push(s, bin, dataDir+"/pulsar.new") != nil {
			return
		}
		if got := devSum(s, dataDir+"/pulsar.new"); want != got {
			_ = shTo(os.Stdout, s, "rm -f "+dataDir+"/pulsar.new")
			fmt.Fprintf(os.Stderr, "%s: push corrupted (checksum mismatch), nothing changed\n", s)
			return
		}
	}
	// Scripts: a running shell reads its script incrementally, so replace them atomically (new inode).
	// A running supervisor keeps executing the old inode: when supervise.sh changed it is reloaded.
	if devSum(s, dataDir+"/supervise.sh") == localSumFor(s, filepath.Join(deploy, "supervise.sh")) {
		supChanged = false
	}
	for _, f := range []string{"boot.sh", "supervise.sh", "ctl.sh"} {
		if push(s, filepath.Join(deploy, f), dataDir+"/"+f+".new") != nil {
			return
		}
	}
	if shTo(os.Stdout, s, "cd "+dataDir+" && chmod 755 boot.sh.new supervise.sh.new ctl.sh.new &&\n"+
		"     mv boot.sh.new boot.sh && mv supervise.sh.new supervise.sh && mv ctl.sh.new ctl.sh") != nil {
		return
	}
	if changed {
		if shTo(os.Stdout, s, "cd "+dataDir+" && chmod 755 pulsar.new &&\n"+
			"       { [ ! -f pulsar ] || cp -p pulsar pulsar.prev; } &&\n"+
			"       mv pulsar.new pulsar && sync") != nil {
			return
		}
	} else {
		_ = shTo(os.Stdout, s, "sync")
	}

	if zigbee && (!deployRadioImage(s, "zigbee-coordinator.ybl") || !deployRadioImage(s, "zigbee-router.ybl")) {
		return
	}

	if code == "irbisA" && (!deployIrbisAudio(s) || !deployGlibc(s) || !deployIrbisOem(s)) {
		return
	}

	if opts.ssh {
		fmt.Printf("== %s: dropbear\n", s)
		if shTo(os.Stdout, s, "mkdir -p /data/dropbear") != nil ||
			push(s, filepath.Join(sshOut, "dropbearmulti"), "/data/dropbear/dropbearmulti.new") != nil ||
			push(s, filepath.Join(sshOut, "sftp-server"), "/data/dropbear/sftp-server.new") != nil ||
			push(s, filepath.Join(root, "ssh", "start.sh"), "/data/dropbear/start.sh") != nil {
			return
		}
		// Running binaries stay busy; mv replaces them atomically.
		if shTo(os.Stdout, s, "cd /data/dropbear && chmod 755 dropbearmulti.new sftp-server.new start.sh &&\n"+
			"       mv dropbearmulti.new dropbearmulti && mv sftp-server.new sftp-server && sync") != nil {
			return
		}
	}

	if opts.ssh {
		_ = shTo(os.Stdout, s, "pidof dropbear >/dev/null || /data/dropbear/start.sh </dev/null >/dev/null 2>&1 &")
	}
	return changed, supChanged, true
}

// deployRadioImage copies one of pulsar's radio firmware images, read by the daemon only when the
// Zigbee role that needs it is chosen. A checkout without radio/build.sh output keeps the installed
// one.
func deployRadioImage(s, name string) bool {
	img := filepath.Join(root, "radio", "out", name)
	if fi, err := os.Stat(img); err != nil || fi.Size() == 0 {
		fmt.Println("radio/out/" + name + " missing (radio/build.sh), not copied")
		return true
	}
	want := localSumFor(s, img)
	if want != "" && want == devSum(s, dataDir+"/"+name) {
		return true
	}
	if push(s, img, dataDir+"/"+name+".new") != nil {
		return false
	}
	if got := devSum(s, dataDir+"/"+name+".new"); got != want {
		_ = shTo(os.Stdout, s, "rm -f "+dataDir+"/"+name+".new")
		fmt.Fprintf(os.Stderr, "%s: push corrupted (checksum mismatch), %s not replaced\n", s, name)
		return false
	}
	return shTo(os.Stdout, s, "cd "+dataDir+" && mv "+name+".new "+name+" && sync") == nil
}

// bootAutostart makes initPath current, then removes the hooks older installers added to
// rootkeeper.sh. note: autostart could not be written (said in the final summary); lost: nothing
// starts pulsar at boot; !ok: station error.
func bootAutostart(s, ts, code string) (note string, lost, ok bool) {
	// Dropbear is installed by --ssh, in this run or an earlier one: never drop its autostart.
	_, err := sh(s, "[ -x /data/dropbear/start.sh ]")
	want := initScript(err == nil)
	wantSum := sumAsDevice(s, want)
	if have := devSum(s, initPath); have != "" && have == wantSum {
		fmt.Println(initPath + " up to date")
	} else {
		reason, ok := writeInitScript(s, want, code)
		if !ok {
			return "", false, false
		}
		if reason != "" {
			fmt.Printf("%s not written: %s\n", initPath, reason)
			note, lost := autostartFallback(s, reason)
			return note, lost, true
		}
	}
	if isSSH(s) {
		// rootkeeper.sh belongs to the Yandex root method; an ssh station has none.
		return "", false, true
	}
	return "", false, migrateRootkeeper(s, ts)
}

// sumAsDevice is want's checksum in the format devSum reports for this device.
func sumAsDevice(s, want string) string {
	if devHasSha256(s) {
		sum := sha256.Sum256([]byte(want))
		return "sha256:" + hex.EncodeToString(sum[:])
	}
	return fmt.Sprintf("size:%d", len(want))
}

// writeInitScript writes want to initPath. reason != "": refused, nothing written (protected or
// unreadable rootfs, remount failed); !ok: station error, reported on stderr.
func writeInitScript(s, want, code string) (reason string, ok bool) {
	wantSum := sumAsDevice(s, want)
	mounts, err := sh(s, "cat /proc/mounts")
	if err != nil {
		return "cannot read /proc/mounts", true
	}
	rw, found := rootMountRW(mounts)
	if !found {
		return "/ not found in /proc/mounts", true
	}
	cmdline, err := sh(s, "cat /proc/cmdline")
	if err != nil {
		return "cannot read /proc/cmdline", true
	}
	// The pipe keeps ls from formatting for the pty (columns, colour).
	mapper, _ := sh(s, "ls /dev/mapper 2>/dev/null | cat")
	// The second path is what must share the root filesystem with /etc: the quasar directory on
	// the Yandex Stations, / itself elsewhere (Irbis A: no /system/vendor).
	second := "/system/vendor/quasar"
	if code == "irbisA" {
		second = "/"
	}
	df, err := sh(s, "df /etc "+second)
	if err != nil {
		return "df /etc " + second + " failed", true
	}
	if r := rootfsGuardLabeled(cmdline, mapper, df, second); r != "" {
		return r, true
	}
	fmt.Println("rootfs: no dm-verity or device-mapper, /etc and " + second + " on the root filesystem")

	tmp, err := os.MkdirTemp("", "pulsar-install")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return "", false
	}
	defer os.RemoveAll(tmp)
	file := filepath.Join(tmp, "S99zpulsar")
	if err := os.WriteFile(file, []byte(want), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return "", false
	}
	src := dataDir + "/S99zpulsar.new"
	if push(s, file, src) != nil {
		return "", false
	}
	defer sh(s, "rm -f "+src)
	// Checked by the shell that runs it at boot (busybox sh).
	if _, err := sh(s, "sh -n "+src); err != nil {
		fmt.Fprintf(os.Stderr, "%s: %s fails sh -n on the device, not installed\n", s, src)
		return "", false
	}
	if !rw {
		// Whatever happened in between, / must not stay read-write.
		defer func() {
			m, err := sh(s, "cat /proc/mounts")
			if now, _ := rootMountRW(m); err == nil && !now {
				return
			}
			_, _ = sh(s, "mount -o remount,ro /")
			m, err = sh(s, "cat /proc/mounts")
			if now, _ := rootMountRW(m); err == nil && !now {
				return
			}
			fmt.Fprintf(os.Stderr, "%s: the root filesystem is still mounted read-write: run \"mount -o remount,ro /\" on the station or reboot it\n", s)
			ok = false
		}()
	}
	out, err := shRaw(s, writeInitCmd(src, want, !rw, devHasSha256(s)))
	if strings.Contains(out, "remount-rw-failed") {
		return "mount -o remount,rw / failed", true
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s: writing %s failed:\n%s\n", s, initPath, out)
		return "", false
	}
	if got := devSum(s, initPath); got != wantSum {
		fmt.Fprintf(os.Stderr, "%s: %s on the device differs from the checked file\n", s, initPath)
		return "", false
	}
	fmt.Printf("wrote %s:\n", initPath)
	for _, l := range splitLines(want) {
		fmt.Println("  " + l)
	}
	return "", true
}

// writeInitCmd is one device command, so the read-write window never spans two adb calls. The
// ignored HUP lets it finish if adb drops; the dot name keeps rcS's S??* glob from running a
// partial file. The subshell keeps exit from skipping shRaw's status echo. Devices without
// sha256sum verify the copy by its size.
func writeInitCmd(src, content string, remount, hasSha bool) string {
	sum := sha256.Sum256([]byte(content))
	verify := fmt.Sprintf(`{ set -- $(sha256sum $T); [ "$1" = %s ]; }`, hex.EncodeToString(sum[:]))
	if !hasSha {
		verify = fmt.Sprintf(`[ "$(wc -c < $T)" = %d ]`, len(content))
	}
	l := []string{"( trap '' HUP INT"}
	if remount {
		l = append(l, "mount -o remount,rw / || { echo remount-rw-failed; exit 1; }")
	}
	l = append(l,
		"T=/etc/init.d/.S99zpulsar.tmp",
		"cp "+src+" $T && chmod 755 $T && "+verify+" && mv $T "+initPath+" && sync",
		"rc=$?",
		"rm -f $T")
	if remount {
		l = append(l, "mount -o remount,ro / || rc=1")
	}
	l = append(l, "exit $rc )")
	return strings.Join(l, "\n")
}

// deployIrbisOem copies the default stop_oem.sh / start_oem.sh (the stock-stack hooks of
// boot.sh / ctl.sh / supervise.sh) — only when the device has none, so on-device edits are
// never clobbered.
func deployIrbisOem(s string) bool {
	for _, name := range []string{"stop_oem.sh", "start_oem.sh"} {
		if _, err := sh(s, "[ -e "+dataDir+"/"+name+" ]"); err == nil {
			continue // keep the device's own version
		}
		local := filepath.Join(root, "satellite", "deploy", "irbis", name)
		if _, err := os.Stat(local); err != nil {
			fmt.Fprintf(os.Stderr, "%s: %v\n", s, err)
			return false
		}
		fmt.Printf("== %s: %s (default, edit on the device as needed)\n", s, name)
		if push(s, local, dataDir+"/"+name+".new") != nil {
			return false
		}
		if shTo(os.Stdout, s, "chmod 755 "+dataDir+"/"+name+".new && mv "+dataDir+"/"+name+".new "+dataDir+"/"+name) != nil {
			return false
		}
	}
	return true
}

// deployGlibc copies the armhf glibc sidecar (satellite/deploy/glibc/out/, made by fetch.py,
// git-ignored and optional): the Irbis A's firmware glibc (2.23) is older than the daemon
// binary's floor, so supervise.sh runs the daemon through the sidecar's loader. No payload
// directory: skipped silently (a firmware with a new enough libc runs the binary directly).
func deployGlibc(s string) bool {
	dir := filepath.Join(root, "satellite", "deploy", "glibc", "out")
	entries, err := os.ReadDir(dir)
	if err != nil {
		return true
	}
	fmt.Printf("== %s: glibc sidecar (%s/glibc)\n", s, dataDir)
	if shTo(os.Stdout, s, "mkdir -p "+dataDir+"/glibc") != nil {
		return false
	}
	for _, e := range entries {
		if e.IsDir() {
			continue
		}
		local := filepath.Join(dir, e.Name())
		remote := dataDir + "/glibc/" + e.Name()
		want := localSumFor(s, local)
		if want != "" && want == devSum(s, remote) {
			continue
		}
		if push(s, local, remote+".new") != nil {
			return false
		}
		if got := devSum(s, remote+".new"); got != want {
			_ = shTo(os.Stdout, s, "rm -f "+remote+".new")
			fmt.Fprintf(os.Stderr, "%s: push corrupted (checksum mismatch), glibc/%s not replaced\n", s, e.Name())
			return false
		}
		if shTo(os.Stdout, s, "chmod 755 "+remote+".new && mv "+remote+".new "+remote) != nil {
			return false
		}
	}
	// The loader runs the daemon through this symlink (comm stays "pulsar" for pidof).
	if shTo(os.Stdout, s, "cd "+dataDir+"/glibc && ln -sf ld-linux-armhf.so.3 pulsar && sync") != nil {
		return false
	}
	return true
}

// deployIrbisAudio writes the daemon's ALSA configuration on an Irbis A: the marked pulsar
// block into /root/.asoundrc (a previous block is replaced, everything else is kept, the
// original file is backed up once as .asoundrc.pulsar-bak), and the earcon WAVs into
// /data/pulsar/sounds (the Model's sound_dir).
func deployIrbisAudio(s string) bool {
	block, err := os.ReadFile(filepath.Join(root, "satellite", "deploy", "irbis", "asoundrc"))
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s: %v\n", s, err)
		return false
	}
	tmp, err := os.MkdirTemp("", "pulsar-install")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return false
	}
	defer os.RemoveAll(tmp)
	f := filepath.Join(tmp, "asoundrc-block")
	if err := os.WriteFile(f, block, 0o644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return false
	}
	fmt.Printf("== %s: ALSA config (/root/.asoundrc)\n", s)
	if push(s, f, "/tmp/pulsar-asoundrc.new") != nil {
		return false
	}
	script := `F=/root/.asoundrc
[ -e "$F" ] || : > "$F"
[ -e "$F.pulsar-bak" ] || cp "$F" "$F.pulsar-bak"
{ sed '/^# --- pulsar begin ---$/,/^# --- pulsar end ---$/d' "$F"; cat /tmp/pulsar-asoundrc.new; } > "$F.tmp" &&
  mv "$F.tmp" "$F" && rm -f /tmp/pulsar-asoundrc.new && sync`
	if shTo(os.Stdout, s, script) != nil {
		return false
	}
	fmt.Printf("== %s: earcons (%s/sounds)\n", s, dataDir)
	if shTo(os.Stdout, s, "mkdir -p "+dataDir+"/sounds") != nil {
		return false
	}
	for _, name := range []string{"spotter_start.wav", "spotter_cancel.wav", "timer.wav", "error.wav", "volume_sound.wav"} {
		local := filepath.Join(root, "satellite", "deploy", "irbis", "sounds", name)
		remote := dataDir + "/sounds/" + name
		want := localSumFor(s, local)
		if want == "" {
			fmt.Fprintf(os.Stderr, "%s: %s missing from the payload\n", s, name)
			return false
		}
		if want == devSum(s, remote) {
			continue
		}
		if push(s, local, remote+".new") != nil {
			return false
		}
		if got := devSum(s, remote+".new"); got != want {
			_ = shTo(os.Stdout, s, "rm -f "+remote+".new")
			fmt.Fprintf(os.Stderr, "%s: push corrupted (checksum mismatch), %s not replaced\n", s, name)
			return false
		}
		if shTo(os.Stdout, s, "mv "+remote+".new "+remote) != nil {
			return false
		}
	}
	return true
}

// autostartFallback tells, after initPath could not be written, what still starts pulsar at boot.
func autostartFallback(s, reason string) (note string, lost bool) {
	if _, err := sh(s, "grep -qxF '"+hookSat+"' "+initPath); err == nil {
		return fmt.Sprintf("%s not updated (%s); the installed one still starts pulsar at boot", initPath, reason), false
	}
	if rk, exists, ok := readRootkeeper(s); ok && exists && hookActive(rk) {
		if _, err := sh(s, "grep -q rootkeeper.sh /system/vendor/quasar/onboot.sh"); err == nil {
			return fmt.Sprintf("%s not written (%s); pulsar still starts at boot through the old hook in %s", initPath, reason, rkPath), false
		}
	}
	return fmt.Sprintf("autostart NOT installed (%s): after a reboot pulsar does not start by itself and the Yandex stack runs; %s/ctl.sh enable starts it again", reason, dataDir), true
}

// readRootkeeper returns rkPath's content; exists is false when there is no such file.
func readRootkeeper(s string) (rk string, exists, ok bool) {
	if _, err := sh(s, "[ -f "+rkPath+" ]"); err != nil {
		return "", false, true
	}
	tmp, err := os.MkdirTemp("", "pulsar-install")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return "", true, false
	}
	defer os.RemoveAll(tmp)
	f := filepath.Join(tmp, "rk")
	if pull(s, rkPath, f) != nil {
		return "", true, false
	}
	b, err := os.ReadFile(f)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return "", true, false
	}
	return string(b), true, true
}

// migrateRootkeeper removes the hook lines older installers added to rootkeeper.sh (now in
// initPath), keeping a backup. Never creates or deletes the file: it belongs to the root method.
func migrateRootkeeper(s, ts string) bool {
	rk, exists, ok := readRootkeeper(s)
	if !ok || !exists {
		return ok
	}
	out, removed := unhookRootkeeper(rk)
	if removed == 0 {
		return true
	}
	// Nothing but our hook lines and the blank separators we added may go.
	if bad := checkRootkeeper(out, rk); bad != "" {
		fmt.Fprintf(os.Stderr, "%s: removing the pulsar lines would change more of %s, not written:\n%s\n", s, rkPath, bad)
		return false
	}
	tmp, err := os.MkdirTemp("", "pulsar-install")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return false
	}
	defer os.RemoveAll(tmp)
	newFile := filepath.Join(tmp, "rk.new")
	if err := os.WriteFile(newFile, []byte(out), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return false
	}
	if push(s, newFile, rkPath+".new") != nil {
		return false
	}
	if _, err := sh(s, "sh -n "+rkPath+".new"); err != nil {
		_, _ = sh(s, "rm -f "+rkPath+".new")
		fmt.Fprintf(os.Stderr, "%s: %s.new fails sh -n on the device, not written\n", s, rkPath)
		return false
	}
	bak := rkPath + ".bak-" + ts
	if shTo(os.Stdout, s, "cp -p "+rkPath+" "+bak+" && chmod 755 "+rkPath+".new && mv "+rkPath+".new "+rkPath+" && sync") != nil {
		return false
	}
	if got, _ := sh(s, "sha256sum "+rkPath); firstField(got) != sha256File(newFile) {
		fmt.Fprintf(os.Stderr, "%s: %s on the device differs from the checked file\n", s, rkPath)
		return false
	}
	fmt.Printf("removed the old pulsar lines from %s, diff against %s on the device:\n", rkPath, bak)
	d, _ := shRaw(s, "diff "+bak+" "+rkPath)
	for _, l := range splitLines(d) {
		fmt.Println("  " + l)
	}
	return true
}

// healthCheck runs after a (re)start, at ~10 s and again at ~30 s: the process is alive and, in
// the log after the install marker, there is exactly one `=== start` and no crash report, exit or
// supervisor message (a binary that crashes and is restarted in between would otherwise look
// healthy). On failure, offer to put the previous binary back.
func healthCheck(s string, changed bool, mark string) bool {
	var log string
	bad, unknown := false, false
	for round := 1; round <= 2; round++ {
		if round == 1 {
			time.Sleep(10 * time.Second)
		} else {
			time.Sleep(20 * time.Second)
		}
		// The log may have been copy-truncated by supervise.sh meanwhile: the older half is in .1.
		full, _ := sh(s, "cat /tmp/pulsar.log.1 /tmp/pulsar.log 2>/dev/null | tail -n 1500")
		log, _ = sh(s, "tail -n 600 /tmp/pulsar.log")
		if !strings.Contains(log, mark) {
			if strings.Contains(full, mark) {
				log = full
			} else {
				log = ""
			}
		}
		if log == "" {
			log = "(install marker not found in the log, even in .1)"
			unknown = true
			break
		}
		// The old daemon's exit (and the restart) land after the marker: judge only what follows
		// the first `=== start` after it, which is the new daemon's.
		_, log, _ = strings.Cut(log, mark)
		i := strings.Index(log, "=== start")
		if i < 0 {
			bad = true
			log = "(no start after the install marker)" + log
			break
		}
		log = log[i:]
		n := 0
		for _, l := range strings.Split(log, "\n") {
			if strings.HasPrefix(l, "=== start") {
				n++
			}
		}
		if shTo(os.Stdout, s, "pidof pulsar >/dev/null") != nil || n != 1 ||
			strings.Contains(log, "=== CRASH") || strings.Contains(log, "=== exit") || strings.Contains(log, "=== supervise") {
			bad = true
			break
		}
	}
	if unknown {
		fmt.Fprintf(os.Stderr, "%s: health check UNKNOWN: %s; not rolling back automatically\n", s, log)
		return false
	}
	if !bad {
		fmt.Printf("%s: health check ok\n", s)
		return true
	}
	fmt.Fprintf(os.Stderr, "%s: health check FAILED, log:\n", s)
	lines := strings.Split(log, "\n")
	if len(lines) > 15 {
		lines = lines[len(lines)-15:]
	}
	fmt.Fprintln(os.Stderr, strings.Join(lines, "\n"))
	if changed && shTo(os.Stdout, s, "[ -f "+dataDir+"/pulsar.prev ]") == nil {
		ans := "y"
		if !opts.yes {
			ans = readLine("roll back to the previous binary? [Y/n] ")
		}
		if !strings.HasPrefix(ans, "n") && !strings.HasPrefix(ans, "N") {
			_ = shTo(os.Stdout, s, "cd "+dataDir+" && cp -p pulsar.prev pulsar.rb && mv pulsar pulsar.bad &&\n"+
				"        mv pulsar.rb pulsar && sync && ./ctl.sh restart")
			fmt.Fprintf(os.Stderr, "%s: rolled back (bad binary kept as %s/pulsar.bad)\n", s, dataDir)
		}
	}
	return false
}
