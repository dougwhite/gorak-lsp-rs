"""Propose a published gorak release without running upstream code."""

import argparse
import base64
import json
import os
import re
import subprocess
import tomllib
from pathlib import Path
from urllib.parse import quote

UPSTREAM = "dougwhite/gorak"
ROOT = Path(__file__).resolve().parent.parent


def version(tag: str) -> tuple:
    match = re.fullmatch(
        r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?",
        tag,
    )
    if not match:
        raise ValueError(f"Not a supported release tag: {tag}")
    prerelease = match[4]
    parts = []
    for part in prerelease.split(".") if prerelease else []:
        if part.isdigit() and len(part) > 1 and part.startswith("0"):
            raise ValueError(f"Invalid numeric prerelease identifier: {tag}")
        parts.append((0, int(part)) if part.isdigit() else (1, part))
    return (*map(int, match.group(1, 2, 3)), not prerelease, tuple(parts))


def select_release(
    releases: list[dict], current: str, requested: str = ""
) -> dict | None:
    current_version = version(current)
    eligible = []
    for release in releases:
        if release.get("draft") or not release.get("published_at"):
            continue
        try:
            candidate = version(release["tag_name"])
        except ValueError:
            continue
        if requested and release["tag_name"] != requested:
            continue
        if candidate > current_version:
            eligible.append(release)
    if requested:
        version(requested)
        if version(requested) <= current_version:
            raise ValueError("Requested release must be newer than the current pin")
        if not eligible:
            raise ValueError("Requested tag is not a published gorak release")
    return max(eligible, key=lambda item: version(item["tag_name"]), default=None)


def api(endpoint: str, paginate: bool = False):
    args = ["gh", "api", endpoint]
    if paginate:
        args += ["--paginate", "--slurp"]
    return json.loads(subprocess.check_output(args, text=True))


def proposed_manifest(original: str, tag: str, upstream: str) -> tuple[str, int, int]:
    local = tomllib.loads(original)
    old = local["source_version"]
    new = tomllib.loads(upstream)["source_version"]
    if type(old) is not int or type(new) is not int or min(old, new) < 1:
        raise ValueError("source_version must be a positive integer")
    if new < old:
        raise ValueError("New release regresses the source contract")
    updated = original
    for key, value in [
        ("gorak_revision", json.dumps(tag)),
        ("source_version", str(new)),
    ]:
        updated, count = re.subn(
            rf"(?m)^({key}[ \t]*=[ \t]*)([^#\r\n]*?)([ \t]*(?:#.*)?)$",
            lambda match: match[1] + value + match[3],
            updated,
        )
        if count != 1:
            raise ValueError(f"Expected one top-level {key}")
    if tomllib.loads(updated) != {
        **local,
        "gorak_revision": tag,
        "source_version": new,
    }:
        raise ValueError("Manifest update changed unrelated values")
    return updated, old, new


def output(name: str, value: str) -> None:
    if location := os.environ.get("GITHUB_OUTPUT"):
        with Path(location).open("a", encoding="utf-8") as destination:
            destination.write(f"{name}={value}\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="")
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--body", type=Path)
    args = parser.parse_args()
    manifest = ROOT / "ecosystem.toml"
    original = manifest.read_text(encoding="utf-8")
    current = tomllib.loads(original)["gorak_revision"]
    pages = api(f"repos/{UPSTREAM}/releases?per_page=100", paginate=True)
    selected = select_release(
        [release for page in pages for release in page], current, args.tag
    )
    output("changed", "false")
    if selected is None:
        print(f"Already current at gorak {current}")
        return
    tag = selected["tag_name"]
    branch = f"automation/gorak-{tag}"
    # Leave open proposals (including human fixes) alone, and respect declined PRs.
    repository = os.environ["GITHUB_REPOSITORY"]
    proposals = api(f"repos/{repository}/pulls?state=all&per_page=100", paginate=True)
    for page in proposals:
        for pr in page:
            if (
                pr["head"]["repo"] is None
                or pr["head"]["repo"]["full_name"] != repository
            ):
                continue
            head = pr["head"]["ref"]
            if head == branch or (
                pr["state"] == "open" and head.startswith("automation/gorak-")
            ):
                print(f"Leaving existing gorak proposal #{pr['number']} unchanged")
                return
    file = api(f"repos/{UPSTREAM}/contents/ecosystem.toml?ref={quote(tag, safe='')}")
    if file.get("type") != "file" or file.get("encoding") != "base64":
        raise ValueError("Release has no readable ecosystem.toml")
    upstream = base64.b64decode(file["content"]).decode("utf-8")
    updated, old, new = proposed_manifest(original, tag, upstream)
    body = (
        f"Update gorak from `{current}` to [{tag}](https://github.com/{UPSTREAM}/releases/tag/{tag}).\n\n"
        f"Source contract: `{old}` → `{new}`. "
        + (
            "**Contract changed: review and adapt the language server before merging.**"
            if old != new
            else "The source contract marker is unchanged."
        )
        + "\n\nThis proposes compatibility; it does not certify it. The existing Windows/Linux "
        "CI fetches the tagged fixtures and tests language-server navigation, diagnostics, "
        "positions and edits. Failing tests require downstream changes. No automatic merge or release.\n"
    )
    print(body)
    if args.body:
        args.body.write_text(body, encoding="utf-8")
    if args.write:
        manifest.write_text(updated, encoding="utf-8")
    output("tag", tag)
    output("branch", branch)
    output("changed", "true")


if __name__ == "__main__":
    main()
