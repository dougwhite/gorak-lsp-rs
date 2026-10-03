"""Fetch the pinned public gorak fixtures for local and CI compatibility tests."""

import subprocess
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parent.parent
pin = tomllib.loads((root / "ecosystem.toml").read_text(encoding="utf-8"))
revision = pin["gorak_revision"]
tag_ref = f"refs/tags/{revision}"
subprocess.run(["git", "check-ref-format", tag_ref], check=True)
checkout = root / ".ci" / "gorak"
checkout.mkdir(parents=True, exist_ok=True)
subprocess.run(["git", "init", str(checkout)], check=True)
subprocess.run(
    ["git", "-C", str(checkout), "fetch", "--depth=1", "https://github.com/dougwhite/gorak.git", f"+{tag_ref}:{tag_ref}"],
    check=True,
)
subprocess.run(["git", "-C", str(checkout), "checkout", "--detach", f"{tag_ref}^{{commit}}"], check=True)
upstream = tomllib.loads((checkout / "ecosystem.toml").read_text(encoding="utf-8"))
if upstream["source_version"] != pin["source_version"]:
    raise SystemExit("Pinned source contract does not match source_version")
print(f"Fetched gorak source contract {pin['source_version']} at {revision}")
