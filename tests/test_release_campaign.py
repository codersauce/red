import copy
from pathlib import Path
import tempfile
import unittest

from scripts.release_campaign import (
    CampaignError,
    DEFAULT_CAMPAIGN,
    SEMVER,
    SOCIAL_LIMITS,
    load_campaign,
    render,
    set_version,
    stories_for_channel,
    validate_campaign,
)


class ReleaseCampaignTest(unittest.TestCase):
    def setUp(self) -> None:
        self.campaign = load_campaign(DEFAULT_CAMPAIGN)

    def test_checked_in_campaign_accepts_next_or_a_resolved_release_version(self) -> None:
        version = self.campaign["version"]

        self.assertTrue(version == "next" or SEMVER.fullmatch(version) is not None)
        self.assertGreaterEqual(len(self.campaign["stories"]), 3)

    def test_preserves_editorial_story_order(self) -> None:
        stories = stories_for_channel(self.campaign, "discord")

        self.assertEqual(stories[0]["id"], "agent-source-walkthroughs")
        self.assertEqual(stories[1]["id"], "inline-agent")
        self.assertEqual(stories[2]["id"], "vim-multicursor")

    def test_rejects_duplicate_story_ids(self) -> None:
        campaign = copy.deepcopy(self.campaign)
        campaign["stories"].append(copy.deepcopy(campaign["stories"][0]))

        with self.assertRaisesRegex(CampaignError, "duplicate story id"):
            validate_campaign(campaign)

    def test_rejects_unknown_channels(self) -> None:
        campaign = copy.deepcopy(self.campaign)
        campaign["stories"][0]["channels"].append("unreviewed")

        with self.assertRaisesRegex(CampaignError, "unsupported channels"):
            validate_campaign(campaign)

    def test_requires_exact_release_version_when_requested(self) -> None:
        with self.assertRaisesRegex(CampaignError, "does not match"):
            validate_campaign(self.campaign, expected_version="0.7.0")

    def test_updates_only_the_resolved_campaign_version(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "campaign.toml"
            path.write_text(DEFAULT_CAMPAIGN.read_text(encoding="utf-8"), encoding="utf-8")

            set_version(path, "0.7.0")
            campaign = load_campaign(path)

            self.assertEqual(campaign["version"], "0.7.0")
            self.assertEqual(campaign["stories"], self.campaign["stories"])

    def test_renders_github_intro_with_prerequisites_and_ordered_stories(self) -> None:
        output = render(self.campaign, "github")

        self.assertIn("### Three things to try", output)
        self.assertIn("### More in this release", output)
        self.assertIn("codex login", output)
        self.assertLess(output.index("Follow an Agent explanation"), output.index("Ctrl-n"))

    def test_social_previews_are_bounded_and_include_destination(self) -> None:
        for channel, limit in SOCIAL_LIMITS.items():
            with self.subTest(channel=channel):
                output = render(self.campaign, channel)

                self.assertLessEqual(len(output.rstrip("\n")), limit)
                self.assertIn("https://github.com/codersauce/red/releases/latest", output)
                self.assertIn("source", output)

        resolved = copy.deepcopy(self.campaign)
        resolved["version"] = "0.7.0"
        for channel, limit in SOCIAL_LIMITS.items():
            self.assertLessEqual(len(render(resolved, channel).rstrip("\n")), limit)
            self.assertIn("Red v0.7.0 is out", render(resolved, channel))

    def test_channel_specific_stories_do_not_leak_into_x_preview(self) -> None:
        output = render(self.campaign, "x")

        self.assertNotIn("reasoning effort", output)

    def test_rejects_boolean_pull_request_numbers(self) -> None:
        campaign = copy.deepcopy(self.campaign)
        campaign["stories"][0]["pull_requests"] = [True]

        with self.assertRaisesRegex(CampaignError, "positive integers"):
            validate_campaign(campaign)


if __name__ == "__main__":
    unittest.main()
