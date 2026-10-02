"""Assemble verified per-platform native artifacts into one release manifest."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import os

source, output = map(Path, sys.argv[1:])
output.mkdir(exist_ok=True)
manifest = {"platforms": {}}
for directory in sorted(source.iterdir()):
    for path in directory.glob("*-x64.json"):
        record = json.loads(path.read_text())
        if "version" in manifest and manifest["version"] != record["version"]:
            raise SystemExit("Mixed release versions")
        manifest["version"] = record["version"]
        for name, checksum in record["files"].items():
            data = (directory / name).read_bytes()
            if hashlib.sha256(data).hexdigest() != checksum:
                raise SystemExit(f"Checksum mismatch: {name}")
            if name == "THIRD_PARTY_NOTICES.md":
                if manifest.get("noticesSha256", checksum) != checksum:
                    raise SystemExit("Native dependency notices differ between platforms")
                manifest["noticesSha256"] = checksum
            else:
                manifest["platforms"][record["platform"]] = {"name": name, "sha256": checksum}
            (output / name).write_bytes(data)
        shutil.copyfile(directory / "LICENSE", output / "LICENSE")
if set(manifest["platforms"]) != {"win32-x64", "linux-x64"}:
    raise SystemExit("Both supported platforms are required")
tag = os.environ.get("GITHUB_REF_NAME", "v" + manifest["version"])
if tag != "v" + manifest["version"]:
    raise SystemExit("Release tag does not match Cargo version")
(output / "release.json").write_text(json.dumps(manifest, indent=2)+"\n")
(output / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in sorted(output.iterdir()) if p.is_file() and p.name != "SHA256SUMS"))
