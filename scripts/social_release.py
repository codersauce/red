#!/usr/bin/env python3
"""Preview release posts safely, or publish explicitly to X and Bluesky."""

from __future__ import annotations

from argparse import ArgumentParser
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import mimetypes
import os
from pathlib import Path
import re
import sys
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlparse
from urllib.request import Request, urlopen
from uuid import uuid4


POST_LIMITS = {"x": 280, "bluesky": 300}
X_API = "https://api.x.com/2"
BLUESKY_API = "https://bsky.social/xrpc"
X_CHUNK_BYTES = 4 * 1024 * 1024
X_IMAGE_BYTES = 5 * 1024 * 1024
X_GIF_BYTES = 15 * 1024 * 1024
X_VIDEO_BYTES = 512 * 1024 * 1024
BLUESKY_IMAGE_BYTES = 1_000_000
REQUEST_TIMEOUT_SECONDS = 30
MAX_PROCESSING_CHECKS = 12
LINK = re.compile(r'https?://[^\s<>"\]]+', re.IGNORECASE)


class SocialReleaseError(ValueError):
    """A reviewed post cannot be validated, previewed, or safely published."""


@dataclass(frozen=True)
class Attachment:
    """One validated local media file, with optional accessible alt text."""

    path: Path
    kind: str
    media_type: str
    size: int
    alt_text: str = ""

    @property
    def x_category(self) -> str:
        if self.kind == "video":
            return "tweet_video"
        return "tweet_gif" if self.media_type == "image/gif" else "tweet_image"


def read_post_text(path: Path, platform: str) -> str:
    """Load reviewed UTF-8 copy and reject empty or oversized public posts."""
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise SocialReleaseError(f"cannot read post text from {path}: {error}") from error

    text = text.replace("\r\n", "\n").replace("\r", "\n").strip()
    if not text:
        raise SocialReleaseError("post text must not be empty")
    if any(ord(character) < 32 and character != "\n" for character in text):
        raise SocialReleaseError("post text contains unsupported control characters")

    limit = POST_LIMITS[platform]
    if len(text) > limit:
        raise SocialReleaseError(
            f"{platform} post exceeds the {limit}-character limit: {len(text)} characters"
        )
    return text


def validate_attachment(
    path: Path, *, kind: str, platform: str, alt_text: str = ""
) -> Attachment:
    """Validate supported media and size before credentials or network access."""
    if platform == "bluesky" and kind == "video":
        raise SocialReleaseError("Bluesky video publishing is not supported")
    if kind == "video" and alt_text:
        raise SocialReleaseError("alt text is currently supported only for images")
    if len(alt_text) > 1_000:
        raise SocialReleaseError("image alt text must not exceed 1,000 characters")

    try:
        if not path.is_file():
            raise SocialReleaseError(f"media file does not exist: {path}")
        size = path.stat().st_size
    except OSError as error:
        raise SocialReleaseError(f"cannot inspect media file {path}: {error}") from error
    if size <= 0:
        raise SocialReleaseError("media file must not be empty")

    media_type, _ = mimetypes.guess_type(path.name)
    allowed = (
        {"image/jpeg", "image/png", "image/gif", "image/webp"}
        if kind == "image"
        else {"video/mp4", "video/webm", "video/quicktime"}
    )
    if media_type not in allowed:
        raise SocialReleaseError(f"unsupported {kind} media type for {path.name}")

    if platform == "bluesky":
        maximum = BLUESKY_IMAGE_BYTES
    elif kind == "video":
        maximum = X_VIDEO_BYTES
    elif media_type == "image/gif":
        maximum = X_GIF_BYTES
    else:
        maximum = X_IMAGE_BYTES
    if size > maximum:
        raise SocialReleaseError(
            f"{platform} {kind} exceeds its {maximum}-byte limit: {size} bytes"
        )

    return Attachment(path=path, kind=kind, media_type=media_type, size=size, alt_text=alt_text)


def link_facets(text: str) -> list[dict[str, Any]]:
    """Return Bluesky link facets indexed in UTF-8 bytes, not characters."""
    facets = []
    for match in LINK.finditer(text):
        url = match.group().rstrip(".,!?;:")
        while url.endswith(")") and url.count(")") > url.count("("):
            url = url[:-1]
        if not url:
            continue
        start = len(text[: match.start()].encode("utf-8"))
        end = start + len(url.encode("utf-8"))
        facets.append(
            {
                "index": {"byteStart": start, "byteEnd": end},
                "features": [{"$type": "app.bsky.richtext.facet#link", "uri": url}],
            }
        )
    return facets


def preview_post(
    platform: str, text: str, attachment: Attachment | None = None
) -> dict[str, Any]:
    """Describe an intended post without inspecting credentials or using the network."""
    preview: dict[str, Any] = {
        "platform": platform,
        "mode": "preview",
        "published": False,
        "text": text,
        "character_count": len(text),
        "character_limit": POST_LIMITS[platform],
    }
    if platform == "bluesky":
        preview["facets"] = link_facets(text)
    if attachment is not None:
        preview["media"] = {
            "path": str(attachment.path),
            "kind": attachment.kind,
            "content_type": attachment.media_type,
            "bytes": attachment.size,
            "alt_text": attachment.alt_text,
        }
    return preview


def _credential(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise SocialReleaseError(f"{name} is required only when --publish is requested")
    if "\r" in value or "\n" in value:
        raise SocialReleaseError(f"{name} must be a single-line credential")
    return value


def _request_json(
    url: str,
    *,
    payload: dict[str, Any] | None = None,
    data: bytes | None = None,
    token: str | None = None,
    content_type: str | None = None,
    method: str = "POST",
) -> dict[str, Any]:
    """Make one bounded request without exposing request bodies or credentials."""
    headers = {"Accept": "application/json", "User-Agent": "red-release-publisher/1"}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if payload is not None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json; charset=utf-8"
    elif content_type is not None:
        headers["Content-Type"] = content_type

    request = Request(url, data=data, headers=headers, method=method)
    try:
        with urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            body = response.read()
    except HTTPError as error:
        endpoint = urlparse(url).path
        raise SocialReleaseError(
            f"request to {endpoint} failed with HTTP {error.code} ({error.reason})"
        ) from error
    except (OSError, URLError) as error:
        endpoint = urlparse(url).path
        raise SocialReleaseError(f"request to {endpoint} failed: {error}") from error

    if not body:
        return {}
    try:
        result = json.loads(body.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise SocialReleaseError("social API returned an invalid JSON response") from error
    if not isinstance(result, dict):
        raise SocialReleaseError("social API returned an unexpected JSON response")
    return result


def _multipart_chunk(attachment: Attachment, chunk: bytes, segment: int) -> tuple[bytes, str]:
    boundary = f"red-release-{uuid4().hex}"
    filename = re.sub(r"[^A-Za-z0-9_.-]", "_", attachment.path.name)
    body = bytearray()
    body.extend(f"--{boundary}\r\n".encode("ascii"))
    body.extend(b'Content-Disposition: form-data; name="segment_index"\r\n\r\n')
    body.extend(f"{segment}\r\n".encode("ascii"))
    body.extend(f"--{boundary}\r\n".encode("ascii"))
    body.extend(
        f'Content-Disposition: form-data; name="media"; filename="{filename}"\r\n'.encode(
            "ascii"
        )
    )
    body.extend(f"Content-Type: {attachment.media_type}\r\n\r\n".encode("ascii"))
    body.extend(chunk)
    body.extend(f"\r\n--{boundary}--\r\n".encode("ascii"))
    return bytes(body), f"multipart/form-data; boundary={boundary}"


def _wait_for_x_media(media_id: str, info: dict[str, Any], token: str) -> None:
    for _ in range(MAX_PROCESSING_CHECKS):
        state = info.get("state")
        if state in (None, "succeeded"):
            return
        if state == "failed":
            raise SocialReleaseError("X rejected the uploaded media during processing")
        if state not in {"pending", "in_progress"}:
            raise SocialReleaseError(f"X returned unknown media processing state {state!r}")

        delay = info.get("check_after_secs", 1)
        if not isinstance(delay, (int, float)) or isinstance(delay, bool):
            delay = 1
        time.sleep(min(max(float(delay), 0), 5))
        status = _request_json(
            f"{X_API}/media/upload?command=STATUS&media_id={quote(media_id, safe='')}",
            token=token,
            method="GET",
        )
        info = status.get("data", {}).get("processing_info", {})
        if not isinstance(info, dict):
            raise SocialReleaseError("X returned invalid media processing details")

    raise SocialReleaseError("X media processing did not finish before the retry limit")


def _upload_x_media(attachment: Attachment, token: str) -> str:
    initialized = _request_json(
        f"{X_API}/media/upload/initialize",
        token=token,
        payload={
            "media_type": attachment.media_type,
            "media_category": attachment.x_category,
            "total_bytes": attachment.size,
        },
    )
    response_data = initialized.get("data", {})
    media_id = response_data.get("id") if isinstance(response_data, dict) else None
    if not isinstance(media_id, str) or re.fullmatch(r"[0-9]{1,19}", media_id) is None:
        raise SocialReleaseError("X media initialization did not return a valid media ID")

    try:
        with attachment.path.open("rb") as source:
            segment = 0
            while chunk := source.read(X_CHUNK_BYTES):
                data, content_type = _multipart_chunk(attachment, chunk, segment)
                _request_json(
                    f"{X_API}/media/upload/{media_id}/append",
                    token=token,
                    data=data,
                    content_type=content_type,
                )
                segment += 1
    except OSError as error:
        raise SocialReleaseError(f"cannot read media file {attachment.path}: {error}") from error

    finalized = _request_json(
        f"{X_API}/media/upload/{media_id}/finalize", token=token, data=b""
    )
    response_data = finalized.get("data", {})
    info = response_data.get("processing_info", {}) if isinstance(response_data, dict) else {}
    if not isinstance(info, dict):
        raise SocialReleaseError("X returned invalid media processing details")
    _wait_for_x_media(media_id, info, token)

    if attachment.alt_text:
        _request_json(
            f"{X_API}/media/metadata",
            token=token,
            payload={"id": media_id, "metadata": {"alt_text": {"text": attachment.alt_text}}},
        )
    return media_id


def publish_x(text: str, attachment: Attachment | None = None) -> dict[str, Any]:
    """Publish one reviewed post using an OAuth 2.0 user-context access token."""
    token = _credential("X_ACCESS_TOKEN")
    post: dict[str, Any] = {"text": text}
    if attachment is not None:
        post["media"] = {"media_ids": [_upload_x_media(attachment, token)]}

    response = _request_json(f"{X_API}/tweets", token=token, payload=post)
    response_data = response.get("data", {})
    post_id = response_data.get("id") if isinstance(response_data, dict) else None
    if not isinstance(post_id, str) or not post_id:
        raise SocialReleaseError("X did not return the published post ID")
    return {
        "platform": "x",
        "mode": "published",
        "published": True,
        "post_id": post_id,
        "url": f"https://x.com/i/web/status/{quote(post_id, safe='')}",
    }


def publish_bluesky(text: str, attachment: Attachment | None = None) -> dict[str, Any]:
    """Create one authenticated Bluesky post with UTF-8-indexed link facets."""
    identifier = _credential("BLUESKY_IDENTIFIER")
    password = _credential("BLUESKY_APP_PASSWORD")
    session = _request_json(
        f"{BLUESKY_API}/com.atproto.server.createSession",
        payload={"identifier": identifier, "password": password},
    )
    access_token = session.get("accessJwt")
    did = session.get("did")
    if not isinstance(access_token, str) or not isinstance(did, str):
        raise SocialReleaseError("Bluesky did not return a usable authenticated session")
    if "\r" in access_token or "\n" in access_token:
        raise SocialReleaseError("Bluesky returned an invalid session credential")

    record: dict[str, Any] = {
        "$type": "app.bsky.feed.post",
        "text": text,
        "createdAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    }
    if facets := link_facets(text):
        record["facets"] = facets

    if attachment is not None:
        try:
            image = attachment.path.read_bytes()
        except OSError as error:
            raise SocialReleaseError(
                f"cannot read media file {attachment.path}: {error}"
            ) from error
        uploaded = _request_json(
            f"{BLUESKY_API}/com.atproto.repo.uploadBlob",
            token=access_token,
            data=image,
            content_type=attachment.media_type,
        )
        blob = uploaded.get("blob")
        if not isinstance(blob, dict):
            raise SocialReleaseError("Bluesky image upload did not return a blob reference")
        record["embed"] = {
            "$type": "app.bsky.embed.images",
            "images": [{"alt": attachment.alt_text, "image": blob}],
        }

    response = _request_json(
        f"{BLUESKY_API}/com.atproto.repo.createRecord",
        token=access_token,
        payload={"repo": did, "collection": "app.bsky.feed.post", "record": record},
    )
    uri = response.get("uri")
    if not isinstance(uri, str) or not uri.startswith("at://"):
        raise SocialReleaseError("Bluesky did not return the published record URI")
    handle = session.get("handle")
    profile = handle if isinstance(handle, str) and handle else did
    record_key = uri.rstrip("/").rsplit("/", 1)[-1]
    return {
        "platform": "bluesky",
        "mode": "published",
        "published": True,
        "uri": uri,
        "url": (
            f"https://bsky.app/profile/{quote(profile, safe='.:')}/post/"
            f"{quote(record_key, safe='')}"
        ),
    }


def main(argv: list[str] | None = None) -> None:
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=tuple(POST_LIMITS), required=True)
    parser.add_argument("--text-file", type=Path, required=True)
    media = parser.add_mutually_exclusive_group()
    media.add_argument("--image", type=Path, help="attach one supported image")
    media.add_argument("--video", type=Path, help="attach one supported X video")
    parser.add_argument("--alt-text", default="", help="accessible image description")
    parser.add_argument(
        "--publish", action="store_true", help="explicitly authorize authenticated publication"
    )
    parser.add_argument("--json", action="store_true", help="emit machine-readable safe output")
    args = parser.parse_args(argv)

    text = read_post_text(args.text_file, args.platform)
    attachment = None
    if args.image is not None:
        attachment = validate_attachment(
            args.image, kind="image", platform=args.platform, alt_text=args.alt_text
        )
    elif args.video is not None:
        attachment = validate_attachment(
            args.video, kind="video", platform=args.platform, alt_text=args.alt_text
        )
    elif args.alt_text:
        raise SocialReleaseError("--alt-text requires an --image attachment")

    if not args.publish:
        result = preview_post(args.platform, text, attachment)
    elif args.platform == "x":
        result = publish_x(text, attachment)
    else:
        result = publish_bluesky(text, attachment)

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    elif args.publish:
        print(f"Published {args.platform} release announcement: {result['url']}")
    else:
        print(f"{args.platform} preview ({len(text)}/{POST_LIMITS[args.platform]}):")
        print(text)


if __name__ == "__main__":
    try:
        main()
    except SocialReleaseError as error:
        raise SystemExit(f"social_release.py: {error}") from error
