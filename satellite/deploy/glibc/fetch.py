#!/usr/bin/env python3
"""Fetches the armhf glibc runtime sidecar for old-glibc ports (the Irbis A: glibc 2.23, while
Rust's gnu targets need >= 2.28 — the daemon then runs through the loader of this sidecar, see
supervise.sh and docs/irbis-a.md).

Downloads the current Debian stable armhf libc6 package and extracts the runtime files the
daemon's process needs (loader, libc, libm, the compat stubs, resolver + NSS modules) into
satellite/deploy/glibc/out/, which pulsar-install copies to /data/pulsar/glibc/ on irbisA
targets. Nothing here is committed to git (out/ is git-ignored); the files are Debian's glibc
build, LGPL-2.1+/GPL-2.0, redistributed unmodified as fetched.

Usage: fetch.py [output_dir]   (needs: python3 >= 3.8 with ssl, network access)
"""

import hashlib
import os
import re
import sys
import tarfile
import io
import urllib.request

POOL = "https://deb.debian.org/debian/pool/main/g/glibc/"
# Files under ./lib/arm-linux-gnueabihf/ of the package that the sidecar keeps.
WANT = [
    "ld-linux-armhf.so.3",
    "libc.so.6",
    "libm.so.6",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libresolv.so.2",
    "libnss_files.so.2",
    "libnss_dns.so.2",
]


def log(*a):
    print(*a, file=sys.stderr, flush=True)


def http(url, timeout=60):
    req = urllib.request.Request(url, headers={"User-Agent": "pulsar-glibc-fetch"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


# Bookworm (2.36): the same glibc generation as the Station Midi images, with a kernel floor
# that old 4.9 firmwares satisfy. Newer suites are only a fallback (GLIBC_DEB= overrides all).
PREFER_SUITE = "deb12u"


def newest_libc6_armhf():
    html = http(POOL).decode(errors="replace")
    vers = set(re.findall(r'href="(libc6_2\.\d+-\d+\+deb\d+u\d+_armhf\.deb)"', html))
    if not vers:
        raise SystemExit(f"no libc6_*_armhf.deb found in {POOL} — pass a manual URL next time")
    preferred = [v for v in vers if PREFER_SUITE in v] or sorted(vers)

    def key(name):
        m = re.search(r"deb(\d+)u(\d+)", name)
        return tuple(int(x) for x in m.groups()) if m else (0, 0)

    return POOL + sorted(preferred, key=key)[-1]


def ar_member(blob, prefix):
    """The first ar member whose name starts with prefix: (name, bytes)."""
    if blob[:8] != b"!<arch>\n":
        raise SystemExit("not an ar archive (.deb)")
    off = 8
    while off + 60 <= len(blob):
        hdr = blob[off : off + 60]
        name = hdr[:16].decode().strip().rstrip("/")
        size = int(hdr[48:58].decode().strip())
        body = off + 60
        if name.startswith(prefix):
            return name, blob[body : body + size]
        off = body + size + (size & 1)
    raise SystemExit(f"member {prefix}* not found in the .deb")


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "out"
    )
    url = os.environ.get("GLIBC_DEB") or newest_libc6_armhf()
    log(f"fetching {url}")
    blob = http(url, timeout=300)
    sha = hashlib.sha256(blob).hexdigest()
    log(f"{len(blob)} bytes, sha256 {sha}")
    name, data = ar_member(blob, "data.tar")
    tf = tarfile.open(fileobj=io.BytesIO(data), mode="r:*")
    prefix = "./lib/arm-linux-gnueabihf/"
    os.makedirs(out, exist_ok=True)
    got = []
    for want in WANT:
        member = None
        for m in tf.getmembers():
            if m.name == prefix + want and m.isfile():
                member = m
                break
        if member is None:
            raise SystemExit(f"{want} not found in {name} ({url})")
        with tf.extractfile(member) as f, open(os.path.join(out, want), "wb") as dst:
            dst.write(f.read())
        os.chmod(os.path.join(out, want), 0o755)
        got.append(want)
    with open(os.path.join(out, "SOURCE"), "w") as f:
        f.write(f"{url}\nsha256:{sha}\n")
    total = sum(os.path.getsize(os.path.join(out, g)) for g in got)
    log(f"{len(got)} files, {total / 1e6:.1f} MB -> {out}")
    log("sanity: the loader must be executable and report its version")
    print("\n".join(got))


if __name__ == "__main__":
    main()
