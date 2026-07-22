#!/usr/bin/env python3

import json
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


RECONCILER = Path(__file__).with_name("reconcile-release.py")
SPEC = importlib.util.spec_from_file_location("reconcile_release", RECONCILER)
RECONCILE_RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RECONCILE_RELEASE)


class ReconcileReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary_directory.name)
        self.plan = self.directory / "plan.json"
        self.observations = self.directory / "observations.json"
        self.missing = self.directory / "missing.json"
        self.plan.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "base_sha": "1" * 40,
                    "target_sha": "2" * 40,
                    "packages": [
                        {
                            "name": "zebra-a",
                            "version": "1.2.3",
                            "manifest_path": "zebra-a/Cargo.toml",
                            "tag": "zebra-a-v1.2.3",
                        },
                        {
                            "name": "zebrad",
                            "version": "4.5.6",
                            "manifest_path": "zebrad/Cargo.toml",
                            "tag": "v4.5.6",
                        },
                    ],
                    "zebrad": {
                        "version": "4.5.6",
                        "tag": "v4.5.6",
                        "prerelease": False,
                        "notes": "## [Zebra 4.5.6]\n\n- Release notes.\n",
                    },
                }
            )
        )

    def tearDown(self):
        self.temporary_directory.cleanup()

    def run_classify(self, observations):
        self.observations.write_text(json.dumps(observations))
        return subprocess.run(
            [
                "python3",
                str(RECONCILER),
                "classify",
                str(self.plan),
                str(self.observations),
                str(self.missing),
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    @staticmethod
    def observation(state, subject, detail=""):
        return {"state": state, "subject": subject, "detail": detail}

    def test_partial_package_state_returns_only_missing_packages(self):
        result = self.run_classify(
            {
                "packages": [
                    self.observation("correct", "zebra-a@1.2.3"),
                    self.observation("missing", "zebrad@4.5.6"),
                ],
                "tags": [
                    self.observation("correct", "zebra-a-v1.2.3"),
                    self.observation("missing", "v4.5.6"),
                ],
                "github_release": self.observation("missing", "v4.5.6"),
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(self.missing.read_text()),
            [
                {
                    "manifest_path": "zebrad/Cargo.toml",
                    "name": "zebrad",
                    "tag": "v4.5.6",
                    "version": "4.5.6",
                }
            ],
        )

    def test_correct_existing_state_is_a_no_op(self):
        result = self.run_classify(
            {
                "packages": [
                    self.observation("correct", "zebra-a@1.2.3"),
                    self.observation("correct", "zebrad@4.5.6"),
                ],
                "tags": [
                    self.observation("correct", "zebra-a-v1.2.3"),
                    self.observation("correct", "v4.5.6"),
                ],
                "github_release": self.observation("correct", "v4.5.6"),
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.missing.read_text()), [])

    def test_contradictory_state_fails_closed(self):
        result = self.run_classify(
            {
                "packages": [
                    self.observation(
                        "contradictory",
                        "zebra-a@1.2.3",
                        "published archive has VCS SHA 333 instead of 222",
                    ),
                    self.observation("missing", "zebrad@4.5.6"),
                ],
                "tags": [
                    self.observation("missing", "zebra-a-v1.2.3"),
                    self.observation("missing", "v4.5.6"),
                ],
                "github_release": self.observation("missing", "v4.5.6"),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "contradictory zebra-a@1.2.3: published archive has VCS SHA 333 instead of 222",
            result.stderr,
        )
        self.assertFalse(self.missing.exists())

    def test_transient_state_has_a_distinct_exit_class(self):
        result = self.run_classify(
            {
                "packages": [
                    self.observation(
                        "transient", "zebra-a@1.2.3", "crates.io returned HTTP 503"
                    ),
                    self.observation("missing", "zebrad@4.5.6"),
                ],
                "tags": [],
                "github_release": self.observation("missing", "v4.5.6"),
            }
        )

        self.assertEqual(result.returncode, 75)
        self.assertIn(
            "transient zebra-a@1.2.3: crates.io returned HTTP 503",
            result.stderr,
        )
        self.assertFalse(self.missing.exists())

    def test_existing_release_before_packages_is_contradictory(self):
        result = self.run_classify(
            {
                "packages": [
                    self.observation("correct", "zebra-a@1.2.3"),
                    self.observation("missing", "zebrad@4.5.6"),
                ],
                "tags": [
                    self.observation("correct", "zebra-a-v1.2.3"),
                    self.observation("correct", "v4.5.6"),
                ],
                "github_release": self.observation("correct", "v4.5.6"),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "contradictory v4.5.6: public GitHub Release exists while expected packages are missing",
            result.stderr,
        )

    def test_existing_draft_release_does_not_block_missing_packages(self):
        draft_release = {
            **self.observation("correct", "v4.5.6"),
            "public": False,
        }
        result = self.run_classify(
            {
                "packages": [
                    self.observation("correct", "zebra-a@1.2.3"),
                    self.observation("missing", "zebrad@4.5.6"),
                ],
                "tags": [
                    self.observation("correct", "zebra-a-v1.2.3"),
                    self.observation("correct", "v4.5.6"),
                ],
                "github_release": draft_release,
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [package["name"] for package in json.loads(self.missing.read_text())],
            ["zebrad"],
        )

    def test_plan_rejects_an_empty_manifest_path(self):
        plan = json.loads(self.plan.read_text())
        plan["packages"][0]["manifest_path"] = ""

        with self.assertRaisesRegex(
            RECONCILE_RELEASE.ContradictionError,
            "manifest path for zebra-a must be repository-relative",
        ):
            RECONCILE_RELEASE.validate_plan(plan)

    def test_publish_recomputes_and_selects_only_missing_packages(self):
        plan = json.loads(self.plan.read_text())
        partial = [
            self.observation("correct", "zebra-a@1.2.3"),
            self.observation("missing", "zebrad@4.5.6"),
        ]
        complete = [
            self.observation("correct", "zebra-a@1.2.3"),
            self.observation("correct", "zebrad@4.5.6"),
        ]

        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "observe_packages",
                side_effect=[partial, complete],
            ),
            mock.patch.object(RECONCILE_RELEASE, "run_command") as run_command,
            mock.patch.object(RECONCILE_RELEASE.time, "sleep"),
        ):
            RECONCILE_RELEASE.publish_missing(plan, self.directory, attempts=3)

        command = run_command.call_args.args[0]
        self.assertEqual(
            command,
            [
                "cargo",
                "+1.91.0",
                "publish",
                "--locked",
                "--package",
                "zebrad",
            ],
        )

    def test_partial_publication_failure_converges_without_republishing(self):
        plan = json.loads(self.plan.read_text())
        both_missing = [
            self.observation("missing", "zebra-a@1.2.3"),
            self.observation("missing", "zebrad@4.5.6"),
        ]
        one_missing = [
            self.observation("correct", "zebra-a@1.2.3"),
            self.observation("missing", "zebrad@4.5.6"),
        ]
        complete = [
            self.observation("correct", "zebra-a@1.2.3"),
            self.observation("correct", "zebrad@4.5.6"),
        ]

        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "observe_packages",
                side_effect=[both_missing, one_missing, complete],
            ),
            mock.patch.object(
                RECONCILE_RELEASE,
                "run_command",
                side_effect=[subprocess.CalledProcessError(1, "cargo"), None],
            ) as run_command,
            mock.patch.object(RECONCILE_RELEASE.time, "sleep"),
        ):
            RECONCILE_RELEASE.publish_missing(plan, self.directory, attempts=3)

        commands = [call.args[0] for call in run_command.call_args_list]
        self.assertEqual(
            commands[0][-4:], ["--package", "zebra-a", "--package", "zebrad"]
        )
        self.assertEqual(commands[1][-2:], ["--package", "zebrad"])

    def test_publication_that_succeeds_on_final_attempt_converges(self):
        plan = json.loads(self.plan.read_text())
        missing = [
            self.observation("correct", "zebra-a@1.2.3"),
            self.observation("missing", "zebrad@4.5.6"),
        ]
        complete = [
            self.observation("correct", "zebra-a@1.2.3"),
            self.observation("correct", "zebrad@4.5.6"),
        ]

        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "observe_packages",
                side_effect=[missing, missing, missing, complete],
            ),
            mock.patch.object(
                RECONCILE_RELEASE,
                "run_command",
                side_effect=[
                    subprocess.CalledProcessError(1, "cargo"),
                    subprocess.CalledProcessError(1, "cargo"),
                    None,
                ],
            ) as run_command,
            mock.patch.object(RECONCILE_RELEASE.time, "sleep"),
        ):
            RECONCILE_RELEASE.publish_missing(plan, self.directory, attempts=3)

        self.assertEqual(run_command.call_count, 3)

    def test_missing_tag_is_created_and_reobserved(self):
        plan = json.loads(self.plan.read_text())
        missing = self.observation("missing", "zebra-a-v1.2.3")
        correct = self.observation("correct", "zebra-a-v1.2.3")

        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "observe_tag",
                side_effect=[missing, correct, correct],
            ),
            mock.patch.object(RECONCILE_RELEASE, "create_tag") as create_tag,
        ):
            RECONCILE_RELEASE.reconcile_tags(plan, self.directory, "example/zebra")

        create_tag.assert_called_once_with(
            plan["packages"][0], plan["target_sha"], "example/zebra"
        )

    def test_wrong_target_tag_is_never_overwritten(self):
        plan = json.loads(self.plan.read_text())
        wrong = self.observation(
            "contradictory",
            "zebra-a-v1.2.3",
            "tag peels to 333 instead of 222",
        )

        with (
            mock.patch.object(RECONCILE_RELEASE, "observe_tag", return_value=wrong),
            mock.patch.object(RECONCILE_RELEASE, "create_tag") as create_tag,
        ):
            with self.assertRaisesRegex(
                RECONCILE_RELEASE.ContradictionError,
                "tag peels to 333 instead of 222",
            ):
                RECONCILE_RELEASE.reconcile_tags(plan, self.directory, "example/zebra")

        create_tag.assert_not_called()

    def test_missing_github_release_is_created_last(self):
        plan = json.loads(self.plan.read_text())
        missing = self.observation("missing", "v4.5.6")

        with (
            mock.patch.object(
                RECONCILE_RELEASE, "observe_github_release", return_value=missing
            ),
            mock.patch.object(
                RECONCILE_RELEASE, "should_make_latest", return_value=True
            ),
            mock.patch.object(RECONCILE_RELEASE, "run_command") as run_command,
        ):
            RECONCILE_RELEASE.reconcile_github_release(
                plan, self.directory, "example/zebra"
            )

        command = run_command.call_args.args[0]
        self.assertEqual(command[:4], ["gh", "release", "create", "v4.5.6"])
        self.assertIn("--verify-tag", command)
        self.assertIn("--latest", command)

    def test_complete_github_release_is_a_no_op(self):
        plan = json.loads(self.plan.read_text())
        existing = {
            **self.observation("correct", "v4.5.6"),
            "public": True,
            "release": {
                "tagName": "v4.5.6",
                "name": "Zebra 4.5.6",
                "isDraft": False,
                "isPrerelease": False,
                "body": plan["zebrad"]["notes"],
            },
        }

        with (
            mock.patch.object(
                RECONCILE_RELEASE, "observe_github_release", return_value=existing
            ),
            mock.patch.object(
                RECONCILE_RELEASE, "should_make_latest", return_value=True
            ),
            mock.patch.object(
                RECONCILE_RELEASE,
                "latest_release_tag",
                return_value="v4.5.6",
            ),
            mock.patch.object(RECONCILE_RELEASE, "run_command") as run_command,
        ):
            RECONCILE_RELEASE.reconcile_github_release(
                plan, self.directory, "example/zebra"
            )

        run_command.assert_not_called()

    def test_library_only_plan_never_creates_a_github_release(self):
        plan = json.loads(self.plan.read_text())
        plan["packages"] = plan["packages"][:1]
        plan["zebrad"] = None

        with mock.patch.object(RECONCILE_RELEASE, "run_command") as run_command:
            RECONCILE_RELEASE.reconcile_github_release(
                plan, self.directory, "example/zebra"
            )

        run_command.assert_not_called()

    def valid_release_pr(self):
        return {
            "state": "MERGED",
            "reviewDecision": "APPROVED",
            "baseRefName": "main",
            "baseRefOid": "1" * 40,
            "headRefName": "release-plz-2026-07-22",
            "headRepository": {"nameWithOwner": "example/zebra"},
            "isCrossRepository": False,
            "mergeCommit": {"oid": "2" * 40},
            "labels": [{"name": "A-release"}],
        }

    def test_release_pr_resolution_accepts_only_the_approved_internal_shape(self):
        resolved = RECONCILE_RELEASE.validate_release_pr(
            "123", "example/zebra", self.valid_release_pr()
        )

        self.assertEqual(
            resolved,
            {"pr_number": "123", "base_sha": "1" * 40, "target_sha": "2" * 40},
        )

    def test_release_pr_resolution_rejects_an_arbitrary_sha(self):
        with self.assertRaisesRegex(
            RECONCILE_RELEASE.ContradictionError,
            "requires a Release PR number",
        ):
            RECONCILE_RELEASE.validate_release_pr(
                "2" * 40, "example/zebra", self.valid_release_pr()
            )

    def test_release_pr_resolution_rejects_each_untrusted_shape(self):
        invalid_cases = {
            "unmerged": ("state", "OPEN"),
            "unapproved": ("reviewDecision", ""),
            "wrong base": ("baseRefName", "next"),
            "wrong branch": ("headRefName", "feature/release"),
            "external": ("isCrossRepository", True),
            "wrong repository": (
                "headRepository",
                {"nameWithOwner": "someone/zebra"},
            ),
            "unlabeled": ("labels", []),
        }
        for description, (field, value) in invalid_cases.items():
            with self.subTest(description=description):
                release_pr = self.valid_release_pr()
                release_pr[field] = value
                with self.assertRaisesRegex(
                    RECONCILE_RELEASE.ContradictionError,
                    "not an approved internal A-release Release PR",
                ):
                    RECONCILE_RELEASE.validate_release_pr(
                        "123", "example/zebra", release_pr
                    )

    def test_historical_stable_release_does_not_replace_a_newer_latest(self):
        plan = json.loads(self.plan.read_text())
        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "stable_release_tags",
                return_value=["v5.0.0"],
            ),
            mock.patch.object(
                RECONCILE_RELEASE,
                "resolve_remote_tags",
                return_value={"v5.0.0": "3" * 40},
            ),
            mock.patch.object(RECONCILE_RELEASE, "is_ancestor", side_effect=[True]),
        ):
            self.assertFalse(
                RECONCILE_RELEASE.should_make_latest(
                    plan, self.directory, "example/zebra"
                )
            )

    def test_latest_comparison_ignores_noncanonical_release_channels(self):
        releases = json.dumps(
            [
                {"tagName": "v6.2.1"},
                {"tagName": "v6.0.0-zcashd-compat.2"},
                {"tagName": "nightly"},
            ]
        )
        completed = subprocess.CompletedProcess([], 0, stdout=releases, stderr="")
        with mock.patch.object(
            RECONCILE_RELEASE, "run_command", return_value=completed
        ):
            self.assertEqual(
                RECONCILE_RELEASE.stable_release_tags("example/zebra"),
                ["v6.2.1"],
            )

    def test_latest_comparison_ignores_unrelated_legacy_release_histories(self):
        plan = json.loads(self.plan.read_text())
        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "stable_release_tags",
                return_value=["v4.3.1"],
            ),
            mock.patch.object(
                RECONCILE_RELEASE,
                "resolve_remote_tags",
                return_value={"v4.3.1": "3" * 40},
            ),
            mock.patch.object(RECONCILE_RELEASE, "is_ancestor", return_value=False),
        ):
            self.assertTrue(
                RECONCILE_RELEASE.should_make_latest(
                    plan, self.directory, "example/zebra"
                )
            )

    def test_historical_release_marked_latest_yields_to_newer_stable_release(self):
        plan = json.loads(self.plan.read_text())
        with (
            mock.patch.object(
                RECONCILE_RELEASE,
                "stable_release_tags",
                return_value=["v4.5.6", "v5.0.0"],
            ),
            mock.patch.object(
                RECONCILE_RELEASE,
                "resolve_remote_tags",
                return_value={
                    "v4.5.6": "2" * 40,
                    "v5.0.0": "3" * 40,
                },
            ),
            mock.patch.object(RECONCILE_RELEASE, "is_ancestor", return_value=True),
        ):
            self.assertFalse(
                RECONCILE_RELEASE.should_make_latest(
                    plan, self.directory, "example/zebra"
                )
            )


if __name__ == "__main__":
    unittest.main()
