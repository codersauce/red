import unittest

from scripts.discord_release import (
    build_payload,
    clean_item,
    ranked_items,
    release_sections,
)


RELEASE = {
    "tagName": "v0.2.4",
    "url": "https://github.com/codersauce/red/releases/tag/v0.2.4",
    "publishedAt": "2026-07-25T14:05:22Z",
    "body": """## What's Changed

### Features

- **neotree:** Add native-style tree scrolling ([fe39cc0](https://example.test/commit))
- **editor:** Highlight matching bracket pairs ([#138](https://example.test/issue)) ([ed72513](https://example.test/commit))

### Bug Fixes

- **lsp:** Prevent pathological batching hangs ([7b126be](https://example.test/commit))

## Contributors

- @new-contributor in #138 (first-time contributor)

## Installation

- This must not become a release highlight
""",
}


class DiscordReleaseTest(unittest.TestCase):
    def test_extracts_release_sections_and_strips_provenance_links(self) -> None:
        sections = release_sections(RELEASE["body"])

        self.assertEqual(
            sections["Features"],
            [
                "**neotree:** Add native-style tree scrolling",
                "**editor:** Highlight matching bracket pairs",
            ],
        )
        self.assertEqual(
            sections["Bug Fixes"],
            ["**lsp:** Prevent pathological batching hangs"],
        )
        self.assertNotIn("new-contributor", str(sections))
        self.assertNotIn("This must not", str(sections))

    def test_builds_a_branded_safe_embed(self) -> None:
        payload = build_payload(RELEASE)
        embed = payload["embeds"][0]

        self.assertEqual(embed["title"], "🚀 Red Editor v0.2.4 is out!")
        self.assertEqual(embed["url"], RELEASE["url"])
        self.assertIn("2 new features and 1 fix", embed["description"])
        self.assertEqual(embed["image"]["url"], "https://getred.dev/editor-dark.png")
        self.assertEqual(payload["allowed_mentions"], {"parse": []})
        self.assertNotIn("content", payload)

    def test_reviewed_campaign_stories_override_commit_ranking(self) -> None:
        campaign = {
            "version": "0.2.4",
            "summary": "Editor-aware agents and Vim-style editing.",
            "stories": [
                {"title": "Jump from agent explanations into source", "channels": ["discord"]},
                {"title": "Review an exact visual selection inline", "channels": ["discord"]},
                {"title": "Internal detail", "channels": ["github"]},
            ],
        }

        payload = build_payload(RELEASE, campaign=campaign)
        embed = payload["embeds"][0]
        highlights = embed["fields"][0]["value"]

        self.assertTrue(embed["description"].startswith(campaign["summary"]))
        self.assertIn("2 new features and 1 fix", embed["description"])
        self.assertLess(highlights.index("agent explanations"), highlights.index("selection inline"))
        self.assertNotIn("Internal detail", highlights)

    def test_rejects_a_campaign_for_another_release(self) -> None:
        with self.assertRaisesRegex(ValueError, "campaign version"):
            build_payload(RELEASE, campaign={"version": "0.9.0"})

    def test_everyone_mention_must_be_enabled_explicitly(self) -> None:
        payload = build_payload(RELEASE, mention_everyone=True)

        self.assertEqual(payload["allowed_mentions"], {"parse": ["everyone"]})
        self.assertTrue(payload["content"].startswith("@everyone"))

    def test_clean_item_preserves_non_provenance_markdown(self) -> None:
        self.assertEqual(
            clean_item("**editor:** Keep `:syntax` working ([abc1234](https://example.test))"),
            "**editor:** Keep `:syntax` working",
        )

    def test_ranks_user_visible_changes_and_keeps_scope_variety(self) -> None:
        items = [
            "**neotree:** Add colors",
            "**neotree:** Add file management actions",
            "**husk:** Add full language server",
            "**git:** Make dashboard interactive",
        ]

        self.assertEqual(
            ranked_items(items)[:3],
            [
                "**git:** Make dashboard interactive",
                "**husk:** Add full language server",
                "**neotree:** Add file management actions",
            ],
        )


if __name__ == "__main__":
    unittest.main()
