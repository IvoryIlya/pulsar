package main

import "testing"

func TestArchOfUsesAvailableUserlandABI(t *testing.T) {
	tests := []struct {
		name, code string
		loaders    []string
		want       string
	}{
		{"cucumber prefers aarch64", "cucumber", []string{"aarch64", "armhf"}, "aarch64"},
		{"cucumber falls back to armhf userspace", "cucumber", []string{"armhf"}, "armhf"},
		{"plum stays armhf", "plum", []string{"aarch64", "armhf"}, "armhf"},
		{"plum without its loader is skipped", "plum", []string{"aarch64"}, ""},
		{"station-mini2 is armhf", "station-mini2", []string{"aarch64", "armhf"}, "armhf"},
		{"station-mini2 without its loader is skipped", "station-mini2", []string{"aarch64"}, ""},
		{"no compatible loader", "cucumber", nil, ""},
		{"unsupported model", "unknown", []string{"aarch64", "armhf"}, ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := archOf(tt.code, tt.loaders); got != tt.want {
				t.Fatalf("archOf(%q, %v) = %q, want %q", tt.code, tt.loaders, got, tt.want)
			}
		})
	}
}

// The devicetree compatible list as `tr '\0' '\n'` leaves it.
func TestStationCode(t *testing.T) {
	cases := []struct {
		compat, code string
		supported    bool
	}{
		// Board-specific variants come first; only the exact supported codename may win.
		{"yandex,cucumber-dvt2\nyandex,cucumber\namlogic, a5", "cucumber", true},
		{"yandex,plum\namlogic, axg", "plum", true},
		// Mini 2: the codename contains a hyphen.
		{"yandex,station-mini2\namlogic, axg", "station-mini2", true},
		// adb's pty line ends and the old separator (spaces) work too.
		{"yandex,station-mini2\r\namlogic, axg\r\n", "station-mini2", true},
		{"yandex,cucumber-dvt2 yandex,cucumber amlogic, a5", "cucumber", true},
		// Some other station: named from its codename, then skipped as unsupported.
		{"yandex,station\namlogic, axg", "station", false},
		{"yandex,cucumber-dvt2\namlogic, a5", "cucumber-dvt2", false},
		{"amlogic, axg", "", false},
	}
	for _, c := range cases {
		code, supported := stationCode(c.compat)
		if code != c.code || supported != c.supported {
			t.Errorf("%q: got (%q, %v), want (%q, %v)", c.compat, code, supported, c.code, c.supported)
		}
	}
}

func TestIrbisModel(t *testing.T) {
	if got := modelName("irbisA"); got != "Irbis A" {
		t.Errorf("modelName(irbisA) = %q", got)
	}
	if got := abis("irbisA"); len(got) != 1 || got[0] != "armhf" {
		t.Errorf("abis(irbisA) = %v", got)
	}
	if hasZigbee("irbisA") {
		t.Error("irbisA has no Zigbee radio")
	}
	// The port's devicetree says "irbis,<board>"; the real Irbis A reports only the generic
	// "amlogic, axg" and needs --model / /data/pulsar/model instead.
	if got, ok := stationCode("irbis,a\namlogic, axg\n"); got != "irbisA" || !ok {
		t.Errorf("stationCode(irbis) = %q, %v", got, ok)
	}
	if got, ok := stationCode("amlogic, axg\n"); got != "" || ok {
		t.Errorf("stationCode(axg) = %q, %v", got, ok)
	}
	if got, ok := stationCode("yandex,station-mini2\namlogic, axg\n"); got != "station-mini2" || !ok {
		t.Errorf("stationCode(mini2) = %q, %v", got, ok)
	}
	if got, _ := stationCode("irbis,\n"); got == "irbisA" {
		t.Error("a bare irbis, prefix must not match")
	}
}

func TestNormalizeSSH(t *testing.T) {
	for in, want := range map[string]string{
		"ssh://root@192.168.1.50":      "ssh://root@192.168.1.50:22",
		"root@192.168.1.50":            "ssh://root@192.168.1.50:22",
		"root@station:2222":            "ssh://root@station:2222",
		"ssh://station":                "ssh://root@station:22",
		"ssh://user@192.168.1.50:2222": "ssh://user@192.168.1.50:2222",
	} {
		if got := normalizeSSH(in); got != want {
			t.Errorf("normalizeSSH(%q) = %q, want %q", in, got, want)
		}
	}
	for _, bad := range []string{"ssh://", "@host", "root@", "ssh://a/b", "root@h:notaport"} {
		if got := normalizeSSH(bad); got != "" {
			t.Errorf("normalizeSSH(%q) = %q, want empty", bad, got)
		}
	}
	tg, err := parseSSH("ssh://root@192.168.1.50:2222")
	if err != nil || tg.user != "root" || tg.host != "192.168.1.50" || tg.port != "2222" {
		t.Errorf("parseSSH = %+v, %v", tg, err)
	}
	if !isSSH("ssh://root@h:22") || isSSH("192.168.1.50:5555") {
		t.Error("isSSH")
	}
}
