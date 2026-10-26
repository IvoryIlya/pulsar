package main

// SSH transport for stations that have no adb (the Irbis A port: the device runs the firmware's
// dropbear, reachable as root over the LAN). A target's serial is "ssh://[user@]host[:port]"
// (default user root, default port 22); sh()/push()/pull() in adb.go dispatch on the prefix.
//
// File copies use scp -O (the legacy protocol): the firmware's dropbear is a multi-call binary
// (dropbearmulti) without an SFTP module, so the SFTP mode of modern OpenSSH scp cannot work.
// The first push to a target also creates the /usr/bin/scp symlink dropbearmulti needs to serve
// the scp applet (it is invoked as "scp -t <path>" over the session).
//
// Key authentication is expected (ssh-copy-id); every command is its own connection, so a
// password login would be asked for again and again.

import (
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
)

type sshTarget struct {
	user, host, port string
}

func isSSH(serial string) bool { return strings.HasPrefix(serial, "ssh://") }

// normalizeSSH brings the user's spellings (root@host, host:2222, ssh://host) to the canonical
// ssh://user@host:port serial. "" on a malformed target.
func normalizeSSH(a string) string {
	rest := strings.TrimPrefix(a, "ssh://")
	user := "root"
	if u, h, ok := strings.Cut(rest, "@"); ok {
		if u == "" {
			return ""
		}
		user, rest = u, h
	}
	host := rest
	port := "22"
	if h, p, ok := strings.Cut(rest, ":"); ok {
		if _, err := strconv.Atoi(p); err != nil || h == "" {
			return ""
		}
		host, port = h, p
	}
	if host == "" || strings.Contains(host, "/") {
		return ""
	}
	return "ssh://" + user + "@" + host + ":" + port
}

func parseSSH(serial string) (sshTarget, error) {
	rest := strings.TrimPrefix(serial, "ssh://")
	user := "root"
	if u, h, ok := strings.Cut(rest, "@"); ok {
		user, rest = u, h
	}
	host, port, _ := strings.Cut(rest, ":")
	if port == "" {
		port = "22"
	}
	if host == "" {
		return sshTarget{}, fmt.Errorf("no host in %s", serial)
	}
	return sshTarget{user, host, port}, nil
}

// sshBase are the options every connection uses: a first connect may add the host key
// (accept-new), but nothing may wait for input besides the authentication itself.
func sshBase(port string) []string {
	return []string{
		"-p", port,
		"-o", "StrictHostKeyChecking=accept-new",
		"-o", "ConnectTimeout=10",
		"-o", "ServerAliveInterval=10",
	}
}

// sshRaw runs cmd on the device and returns its output and an error for connection failures or
// a non-zero remote status (ssh reports it directly, unlike adb shell).
func sshRaw(serial, cmd string) (string, error) {
	t, err := parseSSH(serial)
	if err != nil {
		return "", err
	}
	c := exec.Command("ssh", append(sshBase(t.port), t.user+"@"+t.host, cmd)...)
	out, err := c.Output()
	if err != nil {
		if e, ok := err.(*exec.ExitError); ok {
			return string(out), fmt.Errorf("exit %d", e.ExitCode())
		}
		return "", fmt.Errorf("ssh %s: %w", t.host, err)
	}
	return string(out), nil
}

var scpSymlinks sync.Map // serial -> ensured

// ensureScp makes the device serve the legacy scp protocol: dropbearmulti answers as the scp
// applet only when invoked under that name, and the firmwares ship no symlink for it.
func ensureScp(serial string) error {
	if _, done := scpSymlinks.Load(serial); done {
		return nil
	}
	cmd := `[ -e /usr/bin/scp ] || { for p in /usr/sbin/dropbearmulti /usr/bin/dropbearmulti ` +
		`/sbin/dropbearmulti /usr/sbin/dropbear /usr/bin/dropbear /sbin/dropbear; do ` +
		`[ -x "$p" ] && ln -s "$p" /usr/bin/scp && break; done; }; [ -e /usr/bin/scp ]`
	if _, err := sshRaw(serial, cmd); err != nil {
		return fmt.Errorf("the device has no scp applet (dropbearmulti?) for file copies: %w", err)
	}
	scpSymlinks.Store(serial, true)
	return nil
}

// scpPush copies one file with the legacy scp protocol. No verification here: the callers
// compare checksums (deploy.go devSum) like they do for adb push.
func scpPush(serial, local, remote string) error {
	if err := ensureScp(serial); err != nil {
		return err
	}
	t, err := parseSSH(serial)
	if err != nil {
		return err
	}
	args := []string{"-O", "-P", t.port,
		"-o", "StrictHostKeyChecking=accept-new",
		"-o", "ConnectTimeout=10",
		local, t.user + "@" + t.host + ":" + remote}
	c := exec.Command("scp", args...)
	c.Stderr = os.Stderr
	if err := c.Run(); err != nil {
		return fmt.Errorf("scp %s -> %s: %w", local, remote, err)
	}
	return nil
}

// scpPull copies one file from the device.
func scpPull(serial, remote, local string) error {
	t, err := parseSSH(serial)
	if err != nil {
		return err
	}
	args := []string{"-O", "-P", t.port,
		"-o", "StrictHostKeyChecking=accept-new",
		"-o", "ConnectTimeout=10",
		t.user + "@" + t.host + ":" + remote, local}
	c := exec.Command("scp", args...)
	c.Stderr = os.Stderr
	if err := c.Run(); err != nil {
		return fmt.Errorf("scp %s:%s -> %s: %w", t.host, remote, local, err)
	}
	return nil
}

// sshReachable is the isConnected of an ssh target: one cheap command.
func sshReachable(serial string) bool {
	_, err := sshRaw(serial, "true")
	return err == nil
}
