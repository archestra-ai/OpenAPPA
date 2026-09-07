import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).with_name("appa-oci-tags.py")
SPEC = importlib.util.spec_from_file_location("appa_oci_tags", SCRIPT)
oci_tags = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(oci_tags)

SHA = "0123456789abcdef0123456789abcdef01234567"
OLD = 1
RECENT = 2_600_000
NOW = 3_000_000


def version(package, digest, created, *tags):
    return {
        "package": package,
        "digest": digest,
        "created": created,
        "tags": list(tags),
    }


class OciTagContractTests(unittest.TestCase):
    def test_release_image_tag_is_v_prefixed(self):
        self.assertEqual(oci_tags.release_image_tag("1.2.3"), "v1.2.3")
        self.assertEqual(oci_tags.release_image_tag("1.2.3-rc.1"), "v1.2.3-rc.1")

    def test_release_image_tag_rejects_bare_and_already_prefixed(self):
        with self.assertRaises(ValueError):
            oci_tags.release_image_tag("v1.2.3")
        with self.assertRaises(ValueError):
            oci_tags.release_image_tag("latest")

    def test_rolling_tags_on_pull_request(self):
        self.assertEqual(
            oci_tags.rolling_tags(
                event="pull_request",
                sha=SHA,
                pr_number=12,
            ),
            [f"sha-{SHA[:12]}", "pr-12"],
        )

    def test_rolling_tags_on_main(self):
        self.assertEqual(
            oci_tags.rolling_tags(
                event="push",
                sha=SHA,
                ref="refs/heads/main",
            ),
            [f"sha-{SHA[:12]}", "main", "latest"],
        )

    def test_rolling_tags_on_other_ref_are_sha_only(self):
        self.assertEqual(
            oci_tags.rolling_tags(
                event="workflow_dispatch",
                sha=SHA,
                ref="refs/heads/feat/x",
            ),
            [f"sha-{SHA[:12]}"],
        )

    def test_classify(self):
        self.assertEqual(oci_tags.classify_tag("v1.2.3"), "release")
        self.assertEqual(oci_tags.classify_tag("v1.2.3-rc.1"), "release")
        self.assertEqual(oci_tags.classify_tag(f"sha-{SHA[:12]}"), "ci")
        self.assertEqual(oci_tags.classify_tag("pr-12"), "ci")
        self.assertEqual(oci_tags.classify_tag("main"), "ci")
        self.assertEqual(oci_tags.classify_tag("latest"), "ci")
        self.assertEqual(oci_tags.classify_tag("1.2.3"), "chart")
        self.assertEqual(oci_tags.classify_tag("1.2.3-rc.1"), "chart")
        self.assertEqual(oci_tags.classify_tag("0.2.0"), "chart")
        self.assertEqual(oci_tags.classify_tag("nightly"), "unknown")

    def test_release_digest_is_kept_even_with_a_ci_tag(self):
        versions = [
            version("appa-runtime", "keep-release", OLD, "v1.2.3", "sha-deadbeef"),
        ]
        self.assertEqual(
            oci_tags.deletion_candidates(versions, now=NOW, keep_recent=0),
            [],
        )

    def test_chart_semver_is_never_a_delete_candidate(self):
        versions = [
            version("charts/appa-runtime", "chart", OLD, "1.2.3"),
            version("charts/appa-kagent-demo", "demo", OLD, "1.2.3-rc.1"),
        ]
        self.assertEqual(
            oci_tags.deletion_candidates(versions, now=NOW, keep_recent=0),
            [],
        )

    def test_old_ci_tags_are_candidates_outside_keep_recent(self):
        versions = [
            version("appa-runtime", "old-sha", OLD, f"sha-{SHA[:12]}"),
            version("appa-runtime", "old-pr", OLD, "pr-12"),
        ]
        self.assertEqual(
            oci_tags.deletion_candidates(versions, now=NOW, keep_recent=0),
            ["old-sha", "old-pr"],
        )

    def test_recent_ci_and_current_moving_tags_are_kept(self):
        versions = [
            version("appa-runtime", "fresh-sha", RECENT, f"sha-{SHA[:12]}"),
            version("appa-runtime", "current-main", RECENT, "main", "latest"),
        ]
        self.assertEqual(
            oci_tags.deletion_candidates(versions, now=NOW, keep_recent=0),
            [],
        )

    def test_keep_recent_protects_old_ci_versions(self):
        versions = [
            version("appa-runtime", f"v{i}", OLD + i, f"sha-{i:012x}")
            for i in range(12)
        ]
        self.assertEqual(
            oci_tags.deletion_candidates(versions, now=NOW, keep_recent=10),
            ["v0", "v1"],
        )

    def test_unknown_and_untagged_are_not_selected(self):
        versions = [
            version("appa-runtime", "untagged", OLD),
            version("appa-runtime", "weird", OLD, "nightly"),
        ]
        self.assertEqual(
            oci_tags.deletion_candidates(versions, now=NOW, keep_recent=0),
            [],
        )

    def test_cleanup_policy_file_matches_the_contract(self):
        policies = {item["id"]: item for item in oci_tags.load_policies()}
        self.assertEqual(set(policies), {
            "keep-release-tags",
            "keep-minimum-ci-versions",
            "delete-old-ci-tags",
        })
        self.assertEqual(policies["keep-release-tags"]["action"], "KEEP")
        self.assertEqual(
            policies["keep-release-tags"]["condition"]["tagPrefixes"],
            list(oci_tags.KEEP_TAG_PREFIXES),
        )
        self.assertEqual(
            policies["keep-minimum-ci-versions"]["mostRecentVersions"]["keepCount"],
            oci_tags.KEEP_RECENT,
        )
        self.assertTrue(
            set(oci_tags.IMAGE_PACKAGES).issubset(
                policies["keep-minimum-ci-versions"]["mostRecentVersions"][
                    "packageNamePrefixes"
                ]
            )
        )
        delete = policies["delete-old-ci-tags"]
        self.assertEqual(delete["action"], "DELETE")
        self.assertEqual(
            delete["condition"]["tagPrefixes"],
            list(oci_tags.DELETE_TAG_PREFIXES),
        )
        self.assertEqual(
            delete["condition"]["olderThan"],
            f"{oci_tags.MAX_AGE_SECONDS}s",
        )
        self.assertNotIn("packageNamePrefixes", delete["condition"])


if __name__ == "__main__":
    unittest.main()
