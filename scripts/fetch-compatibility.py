"""Fetch the pinned public Gorak fixtures for local and CI compatibility tests."""

import subprocess
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parent.parent
pin = tomllib.loads((root / "ecosystem.toml").read_text(encoding="utf-8"))
revision = pin["gorak_revision"]
if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
    raise SystemExit("gorak_revision must be a full commit SHA")
checkout = root / ".ci" / "gorak"
checkout.mkdir(parents=True, exist_ok=True)
subprocess.run(["git", "init", str(checkout)], check=True)
subprocess.run(
    ["git", "-C", str(checkout), "fetch", "--depth=1", "https://github.com/dougwhite/gorak.git", revision],
    check=True,
)
subprocess.run(["git", "-C", str(checkout), "checkout", "--detach", revision], check=True)
upstream = tomllib.loads((checkout / "ecosystem.toml").read_text(encoding="utf-8"))
if upstream["source_version"] != pin["source_version"]:
    raise SystemExit("Pinned source contract does not match source_version")
print(f"Fetched Gorak source contract {pin['source_version']} at {revision}")
