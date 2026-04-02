#!/usr/bin/env python3
"""Generate cargo-sources.json from Cargo.lock for Flatpak builds.

Reads the Cargo.lock, extracts every registry dependency, and writes a
JSON array of flatpak-builder source entries that vendor each crate.
"""

import json
import re
import sys
from pathlib import Path


def parse_cargo_lock(path: str) -> list[dict]:
    """Parse Cargo.lock and return list of {name, version, checksum} dicts."""
    text = Path(path).read_text()
    packages = []
    current: dict = {}

    for line in text.splitlines():
        line = line.strip()

        if line == "[[package]]":
            if current.get("checksum") and current.get("source", "").startswith("registry"):
                packages.append(current)
            current = {}
            continue

        m = re.match(r'^(\w+)\s*=\s*"(.+)"$', line)
        if m:
            current[m.group(1)] = m.group(2)

    # Last package
    if current.get("checksum") and current.get("source", "").startswith("registry"):
        packages.append(current)

    return packages


def generate_sources(packages: list[dict]) -> list[dict]:
    """Convert parsed packages to flatpak source entries."""
    sources = []
    for pkg in packages:
        name = pkg["name"]
        version = pkg["version"]
        checksum = pkg["checksum"]
        sources.append({
            "type": "file",
            "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256": checksum,
            "dest": "cargo/vendor",
            "dest-filename": f"{name}-{version}.crate",
        })
    return sources


def main():
    lock_path = sys.argv[1] if len(sys.argv) > 1 else "src-tauri/Cargo.lock"
    out_path = sys.argv[2] if len(sys.argv) > 2 else "cargo-sources.json"

    packages = parse_cargo_lock(lock_path)
    sources = generate_sources(packages)

    Path(out_path).write_text(json.dumps(sources, indent=2) + "\n")
    print(f"Generated {len(sources)} crate entries in {out_path}")


if __name__ == "__main__":
    main()
