"""Build a native release with remapped paths and complete dependency notices."""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parent.parent
os.chdir(ROOT)
platform_name = {("Windows", "AMD64"): "win32-x64", ("Linux", "x86_64"): "linux-x64"}.get((platform.system(), platform.machine()))
if platform_name is None:
    raise SystemExit("Only Windows x64 and Linux x64 releases are currently supported")
version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
env = os.environ.copy()
# Cargo's encoded form preserves Windows paths containing spaces as single arguments.
remaps = [(str(Path.home()), "/build/home"), (str(ROOT), "/build/gorak-lsp-rs")]
cargo_home = env.get("CARGO_HOME")
if cargo_home:
    remaps.append((cargo_home, "/build/cargo"))
flags = [f"--remap-path-prefix={source}={dest}" for source, dest in remaps]
if os.name == "nt":
    flags.append("-Ctarget-feature=+crt-static")
env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
subprocess.run(["cargo", "build", "--locked", "--release", "--bin", "gorak-lsp-rs"], check=True, env=env)
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version=1"]))
nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
required = set()
pending = [metadata["resolve"]["root"]]
while pending:
    identity = pending.pop()
    if identity in required:
        continue
    required.add(identity)
    pending.extend(dep["pkg"] for dep in nodes[identity]["deps"] if any(kind["kind"] != "dev" for kind in dep["dep_kinds"]))
notices = ["# Third-party notices\n"]
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if not package["source"] or package["id"] not in required:
        continue
    directory = Path(package["manifest_path"]).parent
    notices.append(f'\n## {package["name"]} {package["version"]} ({package["license"]})\n')
    licenses = [p for p in directory.iterdir() if p.is_file() and p.name.lower().split(".")[0].split("-")[0].split("_")[0] in {"license", "licence", "copying", "notice"}]
    if not licenses:
        raise SystemExit(f'Missing dependency license: {package["name"]}')
    notices.extend(p.read_text(encoding="utf-8") for p in sorted(licenses))
notices.append((ROOT / "catalogue/PROVENANCE.md").read_text())
output = ROOT / "release"
output.mkdir(exist_ok=True)
name = "gorak-lsp-" + platform_name + (".exe" if os.name == "nt" else "")
binary = ROOT / "target/release" / ("gorak-lsp-rs.exe" if os.name == "nt" else "gorak-lsp-rs")
data = binary.read_bytes()
for source, _ in remaps:
    for value in {source, source.replace("\\", "/")}:
        if value.encode() in data or value.encode("utf-16le") in data:
            raise SystemExit("Release binary contains an unremapped build path")
shutil.copy2(binary, output / name)
(output / "THIRD_PARTY_NOTICES.md").write_text("\n".join(notices), encoding="utf-8", newline="\n")
shutil.copyfile(ROOT / "LICENSE", output / "LICENSE")
files = {name: hashlib.sha256(data).hexdigest(), "THIRD_PARTY_NOTICES.md": hashlib.sha256((output / "THIRD_PARTY_NOTICES.md").read_bytes()).hexdigest()}
(output / f"{platform_name}.json").write_text(json.dumps({"version": version, "platform": platform_name, "files": files}, indent=2)+"\n")
print(f"Built {name} {version}")
