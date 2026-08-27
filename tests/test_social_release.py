from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

from scripts.social_release import (
    BLUESKY_IMAGE_BYTES,
    SocialReleaseError,
    link_facets,
    main,
    preview_post,
    publish_bluesky,
    publish_x,
    read_post_text,
    validate_attachment,
)


class Response:
    def __init__(self, body):
        self.body = json.dumps(body).encode("utf-8")

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self):
        return self.body


class SocialReleaseTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.post = self.root / "post.md"
        self.post.write_text("Red is ready. https://getred.dev\n", encoding="utf-8")

    def image(self, contents=b"image-bytes"):
        path = self.root / "capture.png"
        path.write_bytes(contents)
        return path

    def test_preview_defaults_to_no_network_and_needs_no_credentials(self):
        for platform in ("x", "bluesky"):
            with self.subTest(platform=platform):
                output = io.StringIO()
                with (
                    patch.dict(os.environ, {}, clear=True),
                    patch("scripts.social_release.urlopen") as request,
                    redirect_stdout(output),
                ):
                    main(["--platform", platform, "--text-file", str(self.post), "--json"])

                result = json.loads(output.getvalue())
                self.assertEqual(result["platform"], platform)
                self.assertEqual(result["mode"], "preview")
                self.assertFalse(result["published"])
                self.assertEqual(result["text"], "Red is ready. https://getred.dev")
                request.assert_not_called()

    def test_human_readable_preview_does_not_publish(self):
        output = io.StringIO()
        with patch("scripts.social_release.urlopen") as request, redirect_stdout(output):
            main(["--platform", "x", "--text-file", str(self.post)])

        self.assertIn("x preview", output.getvalue())
        self.assertIn("https://getred.dev", output.getvalue())
        request.assert_not_called()

    def test_missing_x_credentials_fail_without_network(self):
        with patch.dict(os.environ, {}, clear=True):
            with patch("scripts.social_release.urlopen") as request:
                with self.assertRaisesRegex(SocialReleaseError, "X_ACCESS_TOKEN.*--publish"):
                    publish_x("Red is ready")

        request.assert_not_called()

    def test_missing_bluesky_credentials_fail_without_network(self):
        with patch.dict(os.environ, {"BLUESKY_IDENTIFIER": "red.bsky.social"}, clear=True):
            with patch("scripts.social_release.urlopen") as request:
                with self.assertRaisesRegex(SocialReleaseError, "BLUESKY_APP_PASSWORD"):
                    publish_bluesky("Red is ready")

        request.assert_not_called()

    def test_rejects_empty_post(self):
        self.post.write_text("\n  \n", encoding="utf-8")
        with self.assertRaisesRegex(SocialReleaseError, "must not be empty"):
            read_post_text(self.post, "x")

    def test_rejects_oversized_posts_for_each_platform(self):
        for platform, limit in (("x", 280), ("bluesky", 300)):
            with self.subTest(platform=platform):
                self.post.write_text("x" * (limit + 1), encoding="utf-8")
                with self.assertRaisesRegex(SocialReleaseError, f"{limit}-character limit"):
                    read_post_text(self.post, platform)

    def test_rejects_control_characters(self):
        self.post.write_text("Red\x00editor", encoding="utf-8")
        with self.assertRaisesRegex(SocialReleaseError, "control characters"):
            read_post_text(self.post, "bluesky")

    def test_bluesky_link_facets_use_utf8_byte_offsets(self):
        text = "🚀 Red: https://getred.dev/docs. More: https://example.com/a_(b)."
        facets = link_facets(text)

        self.assertEqual(len(facets), 2)
        first = facets[0]
        self.assertEqual(first["index"]["byteStart"], len("🚀 Red: ".encode("utf-8")))
        self.assertEqual(first["features"][0]["uri"], "https://getred.dev/docs")
        self.assertEqual(facets[1]["features"][0]["uri"], "https://example.com/a_(b)")

    def test_bluesky_preview_includes_link_facets(self):
        preview = preview_post("bluesky", "🚀 https://getred.dev")

        self.assertEqual(preview["facets"][0]["index"]["byteStart"], 5)
        self.assertFalse(preview["published"])

    def test_x_post_uses_official_endpoint_and_user_token(self):
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret-x-token"}, clear=True):
            with patch(
                "scripts.social_release.urlopen",
                return_value=Response({"data": {"id": "12345", "text": "Red is ready"}}),
            ) as request:
                result = publish_x("Red is ready")

        sent = request.call_args.args[0]
        self.assertEqual(sent.full_url, "https://api.x.com/2/tweets")
        self.assertEqual(sent.get_header("Authorization"), "Bearer secret-x-token")
        self.assertEqual(json.loads(sent.data), {"text": "Red is ready"})
        self.assertEqual(result["url"], "https://x.com/i/web/status/12345")
        self.assertNotIn("secret-x-token", json.dumps(result))

    def test_x_image_uses_native_chunked_upload_and_alt_text(self):
        attachment = validate_attachment(
            self.image(), kind="image", platform="x", alt_text="Red editor with agent pane"
        )
        responses = [
            Response({"data": {"id": "2468"}}),
            Response({"data": {}}),
            Response({"data": {"id": "2468"}}),
            Response({"data": {"id": "2468"}}),
            Response({"data": {"id": "12345"}}),
        ]
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret-x-token"}, clear=True):
            with patch("scripts.social_release.urlopen", side_effect=responses) as request:
                result = publish_x("Red ships agents", attachment)

        requests = [call.args[0] for call in request.call_args_list]
        self.assertEqual(
            [sent.full_url for sent in requests],
            [
                "https://api.x.com/2/media/upload/initialize",
                "https://api.x.com/2/media/upload/2468/append",
                "https://api.x.com/2/media/upload/2468/finalize",
                "https://api.x.com/2/media/metadata",
                "https://api.x.com/2/tweets",
            ],
        )
        initialized = json.loads(requests[0].data)
        self.assertEqual(initialized["media_category"], "tweet_image")
        self.assertEqual(initialized["total_bytes"], len(b"image-bytes"))
        self.assertIn(b"image-bytes", requests[1].data)
        self.assertIn(b'name="segment_index"', requests[1].data)
        metadata = json.loads(requests[3].data)
        self.assertEqual(metadata["metadata"]["alt_text"]["text"], attachment.alt_text)
        self.assertEqual(json.loads(requests[4].data)["media"], {"media_ids": ["2468"]})
        self.assertTrue(result["published"])

    def test_x_video_streams_multiple_chunks(self):
        video = self.root / "demo.mp4"
        video.write_bytes(b"1234567")
        attachment = validate_attachment(video, kind="video", platform="x")
        responses = [
            Response({"data": {"id": "2468"}}),
            Response({"data": {}}),
            Response({"data": {}}),
            Response({"data": {}}),
            Response({"data": {"id": "2468"}}),
            Response({"data": {"id": "12345"}}),
        ]
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret-x-token"}, clear=True):
            with (
                patch("scripts.social_release.X_CHUNK_BYTES", 3),
                patch("scripts.social_release.urlopen", side_effect=responses) as request,
            ):
                publish_x("Watch Red edit", attachment)

        requests = [call.args[0] for call in request.call_args_list]
        self.assertEqual(json.loads(requests[0].data)["media_category"], "tweet_video")
        self.assertIn(b"123", requests[1].data)
        self.assertIn(b"456", requests[2].data)
        self.assertIn(b"7", requests[3].data)

    def test_x_waits_for_asynchronous_video_processing(self):
        attachment = validate_attachment(self.image(), kind="image", platform="x")
        responses = [
            Response({"data": {"id": "2468"}}),
            Response({"data": {}}),
            Response(
                {"data": {"id": "2468", "processing_info": {"state": "pending", "check_after_secs": 0}}}
            ),
            Response({"data": {"processing_info": {"state": "succeeded"}}}),
            Response({"data": {"id": "12345"}}),
        ]
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret-x-token"}, clear=True):
            with (
                patch("scripts.social_release.time.sleep") as sleep,
                patch("scripts.social_release.urlopen", side_effect=responses) as request,
            ):
                publish_x("Watch Red", attachment)

        status = request.call_args_list[3].args[0]
        self.assertEqual(status.get_method(), "GET")
        self.assertIn("command=STATUS&media_id=2468", status.full_url)
        sleep.assert_called_once_with(0.0)

    def test_x_rejects_failed_media_processing(self):
        attachment = validate_attachment(self.image(), kind="image", platform="x")
        responses = [
            Response({"data": {"id": "2468"}}),
            Response({"data": {}}),
            Response({"data": {"processing_info": {"state": "failed"}}}),
        ]
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret-x-token"}, clear=True):
            with patch("scripts.social_release.urlopen", side_effect=responses):
                with self.assertRaisesRegex(SocialReleaseError, "rejected the uploaded media"):
                    publish_x("Watch Red", attachment)

    def test_http_failures_never_expose_credentials_or_response_body(self):
        response = io.BytesIO(b"server echoed secret-x-token")
        failure = HTTPError("https://api.x.com/2/tweets", 403, "Forbidden", None, response)
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret-x-token"}, clear=True):
            with patch("scripts.social_release.urlopen", side_effect=failure):
                with self.assertRaises(SocialReleaseError) as context:
                    publish_x("Red is ready")

        self.assertIn("HTTP 403", str(context.exception))
        self.assertNotIn("secret-x-token", str(context.exception))

    def test_bluesky_session_and_record_include_utf8_link_facets(self):
        session = {
            "accessJwt": "secret-session-token",
            "refreshJwt": "secret-refresh-token",
            "did": "did:plc:red123",
            "handle": "red.bsky.social",
        }
        uri = "at://did:plc:red123/app.bsky.feed.post/abc123"
        with patch.dict(
            os.environ,
            {"BLUESKY_IDENTIFIER": "red.bsky.social", "BLUESKY_APP_PASSWORD": "secret-app-password"},
            clear=True,
        ):
            with patch(
                "scripts.social_release.urlopen",
                side_effect=[Response(session), Response({"uri": uri, "cid": "bafy123"})],
            ) as request:
                result = publish_bluesky("🚀 Red https://getred.dev")

        login, posted = [call.args[0] for call in request.call_args_list]
        self.assertEqual(login.full_url, "https://bsky.social/xrpc/com.atproto.server.createSession")
        self.assertEqual(json.loads(login.data)["password"], "secret-app-password")
        self.assertEqual(posted.get_header("Authorization"), "Bearer secret-session-token")
        body = json.loads(posted.data)
        self.assertEqual(body["repo"], "did:plc:red123")
        self.assertEqual(body["collection"], "app.bsky.feed.post")
        self.assertEqual(body["record"]["facets"][0]["index"]["byteStart"], 9)
        self.assertTrue(body["record"]["createdAt"].endswith("Z"))
        self.assertEqual(result["url"], "https://bsky.app/profile/red.bsky.social/post/abc123")
        self.assertNotIn("secret", json.dumps(result))

    def test_bluesky_image_upload_embeds_blob_and_alt_text(self):
        attachment = validate_attachment(
            self.image(), kind="image", platform="bluesky", alt_text="Red showing inline assist"
        )
        blob = {"$type": "blob", "ref": {"$link": "bafy123"}, "mimeType": "image/png"}
        session = {"accessJwt": "secret-session-token", "did": "did:plc:red123"}
        uri = "at://did:plc:red123/app.bsky.feed.post/post456"
        with patch.dict(
            os.environ,
            {"BLUESKY_IDENTIFIER": "person@example.test", "BLUESKY_APP_PASSWORD": "secret-password"},
            clear=True,
        ):
            with patch(
                "scripts.social_release.urlopen",
                side_effect=[Response(session), Response({"blob": blob}), Response({"uri": uri})],
            ) as request:
                result = publish_bluesky("Inline help", attachment)

        login, uploaded, posted = [call.args[0] for call in request.call_args_list]
        self.assertIn("createSession", login.full_url)
        self.assertEqual(uploaded.full_url, "https://bsky.social/xrpc/com.atproto.repo.uploadBlob")
        self.assertEqual(uploaded.get_header("Content-type"), "image/png")
        self.assertEqual(uploaded.data, b"image-bytes")
        image = json.loads(posted.data)["record"]["embed"]["images"][0]
        self.assertEqual(image, {"alt": "Red showing inline assist", "image": blob})
        self.assertEqual(result["url"], "https://bsky.app/profile/did:plc:red123/post/post456")
        self.assertNotIn("person@example.test", json.dumps(result))

    def test_rejects_bluesky_video(self):
        video = self.root / "demo.mp4"
        video.write_bytes(b"video")
        with self.assertRaisesRegex(SocialReleaseError, "Bluesky video"):
            validate_attachment(video, kind="video", platform="bluesky")

    def test_rejects_oversized_bluesky_image(self):
        image = self.image(b"x" * (BLUESKY_IMAGE_BYTES + 1))
        with self.assertRaisesRegex(SocialReleaseError, "1000000-byte limit"):
            validate_attachment(image, kind="image", platform="bluesky")

    def test_rejects_unsupported_media(self):
        document = self.root / "not-image.txt"
        document.write_text("not an image", encoding="utf-8")
        with self.assertRaisesRegex(SocialReleaseError, "unsupported image"):
            validate_attachment(document, kind="image", platform="x")

    def test_rejects_alt_text_without_an_image(self):
        with self.assertRaisesRegex(SocialReleaseError, "requires an --image"):
            main(
                [
                    "--platform",
                    "x",
                    "--text-file",
                    str(self.post),
                    "--alt-text",
                    "a missing image",
                ]
            )

    def test_rejects_credentials_with_header_injection(self):
        with patch.dict(os.environ, {"X_ACCESS_TOKEN": "secret\ninjected"}, clear=True):
            with patch("scripts.social_release.urlopen") as request:
                with self.assertRaisesRegex(SocialReleaseError, "single-line credential"):
                    publish_x("Red is ready")

        request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
