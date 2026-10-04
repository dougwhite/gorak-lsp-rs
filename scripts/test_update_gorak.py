import base64
import io
import os
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

import update_gorak as updater


def release(tag, **changes):
    return {
        "tag_name": tag,
        "draft": False,
        "published_at": "2026-10-01T00:00:00Z",
        **changes,
    }


class ReleaseSelection(unittest.TestCase):
    def test_numeric_alpha_and_stable_order(self):
        releases = [
            release(tag) for tag in ["v0.1.0-alpha.9", "v0.1.0-alpha.10", "v0.1.0"]
        ]
        self.assertEqual(
            updater.select_release(releases[:2], "v0.1.0-alpha.1")["tag_name"],
            "v0.1.0-alpha.10",
        )
        self.assertEqual(
            updater.select_release(releases, "v0.1.0-alpha.1")["tag_name"], "v0.1.0"
        )

    def test_drafts_unpublished_and_invalid_tags_ignored(self):
        releases = [
            release("v9.0.0", draft=True),
            release("v8.0.0", published_at=None),
            release("main"),
            release("v0.2.0"),
        ]
        self.assertEqual(
            updater.select_release(releases, "v0.1.0")["tag_name"], "v0.2.0"
        )

    def test_no_downgrade_or_repeat(self):
        self.assertIsNone(
            updater.select_release(
                [release("v0.1.0"), release("v0.2.0-alpha.1")], "v0.2.0"
            )
        )

    def test_manual_tag_must_be_published_and_newer(self):
        releases = [release("v0.2.0"), release("v0.3.0")]
        self.assertEqual(
            updater.select_release(releases, "v0.1.0", "v0.2.0")["tag_name"], "v0.2.0"
        )
        for tag in ["v0.1.0", "v0.4.0", "main", "v0.2.0-alpha.01"]:
            with self.assertRaises(ValueError):
                updater.select_release(releases, "v0.1.0", tag)


class ManifestUpdate(unittest.TestCase):
    def test_contract_change_preserves_comments_and_other_values(self):
        original = (
            'source_version = 1 # contract\ngorak_revision = "v0.1.0"\nother = "kept"\n'
        )
        text, old, new = updater.proposed_manifest(
            original, "v0.2.0", 'source_version = 2\ngorak_cli = "0.2.0"\n'
        )
        self.assertEqual((old, new), (1, 2))
        self.assertEqual(
            text,
            'source_version = 2 # contract\ngorak_revision = "v0.2.0"\nother = "kept"\n',
        )
        self.assertEqual(tomllib.loads(text)["other"], "kept")

    def test_bad_contract_and_contract_regression_rejected(self):
        for contract in ["true", "0", '"2"', "1"]:
            with self.assertRaises(ValueError):
                updater.proposed_manifest(
                    'source_version = 2\ngorak_revision = "v0.1.0"\n',
                    "v0.2.0",
                    f"source_version = {contract}",
                )

    def test_missing_and_nested_marker_rejected(self):
        for original in [
            "source_version = 1\n",
            '[pins]\nsource_version = 1\ngorak_revision = "v0.1.0"\n',
        ]:
            with self.assertRaises((ValueError, KeyError)):
                updater.proposed_manifest(original, "v0.2.0", "source_version = 1")


class Proposal(unittest.TestCase):
    def run_proposal(
        self, *, write=False, prs=None, releases=None, upstream="source_version = 2"
    ):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        original = 'source_version = 1\ngorak_revision = "v0.1.0"\n'
        (root / "ecosystem.toml").write_text(original)
        body = root / "body.md"
        outputs = root / "outputs"
        calls = []

        def api(endpoint, paginate=False):
            calls.append(endpoint)
            if "/releases?" in endpoint:
                return [[release("v0.2.0")] if releases is None else releases]
            if "/pulls?" in endpoint:
                return [prs or []]
            if "/contents/" in endpoint:
                return {
                    "type": "file",
                    "encoding": "base64",
                    "content": base64.b64encode(upstream.encode()).decode(),
                }
            raise AssertionError(endpoint)

        args = ["update_gorak.py", "--body", str(body)] + (["--write"] if write else [])
        with (
            patch.object(updater, "ROOT", root),
            patch.object(updater, "api", side_effect=api),
            patch("sys.argv", args),
            patch.dict(
                os.environ,
                {
                    "GITHUB_REPOSITORY": "dougwhite/gorak-lsp-rs",
                    "GITHUB_OUTPUT": str(outputs),
                },
            ),
        ):
            updater.main()
        return root, original, outputs.read_text(), calls

    def test_dry_preview_does_not_modify_pin(self):
        root, original, outputs, calls = self.run_proposal()
        self.assertEqual((root / "ecosystem.toml").read_text(), original)
        self.assertIn("Contract changed", (root / "body.md").read_text())
        self.assertIn("changed=true", outputs)
        self.assertIn("ref=v0.2.0", calls[-1])

    def test_write_changes_only_manifest(self):
        root, _, _, _ = self.run_proposal(write=True)
        self.assertEqual(
            tomllib.loads((root / "ecosystem.toml").read_text()),
            {"source_version": 2, "gorak_revision": "v0.2.0"},
        )

    def test_preview_on_windows_console_encoding(self):
        with io.TextIOWrapper(io.BytesIO(), encoding="cp1252") as console:
            with patch("sys.stdout", console):
                root, _, outputs, _ = self.run_proposal(write=True)
            console.flush()
            self.assertIn("changed=true", outputs)
            self.assertIn(
                "Source contract: `1` to `2`.",
                (root / "body.md").read_text(encoding="utf-8"),
            )

    def test_existing_open_or_declined_proposals_left_alone(self):
        for state, branch in [
            ("open", "automation/gorak-v0.1.1"),
            ("closed", "automation/gorak-v0.2.0"),
        ]:
            pr = {
                "number": 10,
                "state": state,
                "head": {
                    "ref": branch,
                    "repo": {"full_name": "dougwhite/gorak-lsp-rs"},
                },
            }
            root, original, outputs, calls = self.run_proposal(write=True, prs=[pr])
            self.assertEqual((root / "ecosystem.toml").read_text(), original)
            self.assertNotIn("changed=true", outputs)
            self.assertFalse(any("/contents/" in call for call in calls))

    def test_fork_proposals_do_not_block(self):
        pr = {
            "number": 10,
            "state": "open",
            "head": {
                "ref": "automation/gorak-v0.2.0",
                "repo": {"full_name": "someone/fork"},
            },
        }
        _, _, outputs, _ = self.run_proposal(prs=[pr])
        self.assertIn("changed=true", outputs)

    def test_current_release_is_noop(self):
        root, original, outputs, calls = self.run_proposal(releases=[release("v0.1.0")])
        self.assertEqual((root / "ecosystem.toml").read_text(), original)
        self.assertEqual(outputs, "changed=false\n")
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
