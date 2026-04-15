// Package arp resolves MAC addresses for IPv4 hosts on the local L2 segment.
// Used to give Modbus TCP / HTTP / on-LAN MQTT devices a hardware-stable
// identity even when they don't expose a serial number in their protocol.
//
// Cross-subnet (L3) addresses can't be resolved — the kernel's ARP table
// only contains entries for hosts the box has talked to on the same L2.
// In that case Lookup returns ("", false) and callers should fall back to
// an endpoint-hash identity.
package arp

import (
	"net"
	"os"
	"os/exec"
	"runtime"
	"strings"
	"time"
)

// Lookup resolves the MAC address for an IPv4 address. Best-effort: the
// kernel's ARP cache only knows hosts we've talked to recently, so we
// nudge the cache by sending a single TCP probe to a common port (80) on
// the host before reading. The probe is cheap (50 ms timeout) and silent
// on failure — what matters is the SYN packet itself triggering ARP.
//
// Returns (mac, true) on success or ("", false) if the host can't be
// reached on L2 or isn't on this segment at all.
func Lookup(ipStr string) (string, bool) {
	ip := net.ParseIP(ipStr)
	if ip == nil { return "", false }
	ip4 := ip.To4()
	if ip4 == nil { return "", false }
	// Nudge the ARP cache. We don't care if connect succeeds — the kernel
	// resolves ARP regardless when it tries to send the SYN.
	for _, port := range []string{"80", "502", "1883"} {
		c, _ := net.DialTimeout("tcp", net.JoinHostPort(ipStr, port), 50*time.Millisecond)
		if c != nil { c.Close(); break }
	}
	switch runtime.GOOS {
	case "linux":
		return lookupLinux(ipStr)
	case "darwin":
		return lookupDarwin(ipStr)
	}
	return "", false
}

// lookupLinux parses /proc/net/arp.
//
//	IP address       HW type     Flags       HW address            Mask     Device
//	192.168.1.42     0x1         0x2         aa:bb:cc:dd:ee:ff     *        eth0
func lookupLinux(ipStr string) (string, bool) {
	data, err := readFile("/proc/net/arp")
	if err != nil { return "", false }
	for i, line := range strings.Split(string(data), "\n") {
		if i == 0 || line == "" { continue }
		fields := strings.Fields(line)
		if len(fields) < 4 { continue }
		if fields[0] != ipStr { continue }
		mac := strings.ToLower(fields[3])
		// Incomplete entries show as "00:00:00:00:00:00".
		if mac == "00:00:00:00:00:00" { return "", false }
		return mac, true
	}
	return "", false
}

// lookupDarwin shells out to /usr/sbin/arp. macOS doesn't expose /proc.
func lookupDarwin(ipStr string) (string, bool) {
	out, err := exec.Command("/usr/sbin/arp", "-n", ipStr).CombinedOutput()
	if err != nil { return "", false }
	// Output: "? (192.168.1.42) at aa:bb:cc:dd:ee:ff on en0 ifscope [ethernet]"
	s := string(out)
	idx := strings.Index(s, " at ")
	if idx < 0 { return "", false }
	rest := s[idx+4:]
	end := strings.Index(rest, " ")
	if end < 0 { return "", false }
	mac := strings.ToLower(rest[:end])
	if mac == "(incomplete)" || mac == "no" { return "", false }
	// Normalize single-digit octets (macOS prints "1:2:3:4:5:6")
	parts := strings.Split(mac, ":")
	if len(parts) != 6 { return "", false }
	for i, p := range parts {
		if len(p) == 1 { parts[i] = "0" + p }
	}
	return strings.Join(parts, ":"), true
}

// readFile is a tiny indirection so tests can stub /proc/net/arp.
var readFile = os.ReadFile
