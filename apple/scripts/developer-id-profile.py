#!/usr/bin/env python3
"""Finds or creates the Mac app's Developer ID provisioning profile through the App Store
Connect API, installs it for Xcode, and prints its UUID:

    apple/scripts/developer-id-profile.py BUNDLE_ID CERT_SHA1

The profile is a MAC_APP_DIRECT one for BUNDLE_ID with the Developer ID certificate whose
SHA-1 is CERT_SHA1 (as `security find-identity` prints it). It carries the App ID's
capabilities, among them the associated domains passkeys need. An active profile of that name
with the certificate is reused; others of that name, made for an earlier certificate, are
deleted first. Needs ASC_KEY_ID, ASC_ISSUER_ID and ASC_KEY_PATH (the key's .p8 file).
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import pathlib
import plistlib
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"
NAME = "herder Developer ID"


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def der_to_raw(der: bytes) -> bytes:
    """An ECDSA signature as openssl writes it (DER) to the r || s form JWT's ES256 wants."""
    assert der[0] == 0x30
    i = 2 if der[1] < 0x80 else 2 + (der[1] & 0x7F)
    parts = []
    for _ in range(2):
        assert der[i] == 0x02
        length = der[i + 1]
        parts.append(der[i + 2 : i + 2 + length].lstrip(b"\0").rjust(32, b"\0"))
        i += 2 + length
    return b"".join(parts)


def token() -> str:
    now = int(time.time())
    header = {"alg": "ES256", "kid": os.environ["ASC_KEY_ID"], "typ": "JWT"}
    claims = {
        "iss": os.environ["ASC_ISSUER_ID"],
        "iat": now,
        "exp": now + 1200,
        "aud": "appstoreconnect-v1",
    }
    signing_input = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
    der = subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", os.environ["ASC_KEY_PATH"]],
        input=signing_input.encode(),
        capture_output=True,
        check=True,
    ).stdout
    return f"{signing_input}.{b64url(der_to_raw(der))}"


AUTH = token()


def call(method: str, path: str, query: dict | None = None, body: dict | None = None) -> dict:
    url = f"{API}/{path}"
    if query:
        url += "?" + urllib.parse.urlencode(query)
    request = urllib.request.Request(url, method=method)
    request.add_header("Authorization", f"Bearer {AUTH}")
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, data) as response:
            text = response.read()
    except urllib.error.HTTPError as err:
        sys.exit(f"{method} {path} failed with {err.code}: {err.read().decode()}")
    return json.loads(text) if text else {}


def main() -> None:
    bundle, sha1 = sys.argv[1], sys.argv[2].upper()

    ids = call("GET", "bundleIds", {"filter[identifier]": bundle, "limit": 200})["data"]
    bundle_id = next((i["id"] for i in ids if i["attributes"]["identifier"] == bundle), None)
    if bundle_id is None:
        sys.exit(f"App Store Connect has no App ID {bundle}")

    certificates = call("GET", "certificates", {"limit": 200})["data"]
    certificate = next(
        (
            c["id"]
            for c in certificates
            if c["attributes"].get("certificateContent")
            and hashlib.sha1(base64.b64decode(c["attributes"]["certificateContent"]))
            .hexdigest()
            .upper()
            == sha1
        ),
        None,
    )
    if certificate is None:
        sys.exit(f"App Store Connect has no certificate with SHA-1 {sha1}")

    profile = None
    existing = call(
        "GET",
        "profiles",
        {
            "filter[name]": NAME,
            "filter[profileType]": "MAC_APP_DIRECT",
            "include": "certificates,bundleId",
            "limit": 50,
        },
    )["data"]
    # The name filter may match more than the name: only this exact name is the script's own.
    for candidate in (p for p in existing if p["attributes"]["name"] == NAME):
        relationships = candidate["relationships"]
        fits = (
            candidate["attributes"]["profileState"] == "ACTIVE"
            and relationships["bundleId"]["data"]["id"] == bundle_id
            and certificate in [c["id"] for c in relationships["certificates"]["data"]]
        )
        if fits and profile is None:
            profile = candidate
        else:
            call("DELETE", f"profiles/{candidate['id']}")
    if profile is None:
        profile = call(
            "POST",
            "profiles",
            body={
                "data": {
                    "type": "profiles",
                    "attributes": {"name": NAME, "profileType": "MAC_APP_DIRECT"},
                    "relationships": {
                        "bundleId": {"data": {"type": "bundleIds", "id": bundle_id}},
                        "certificates": {"data": [{"type": "certificates", "id": certificate}]},
                    },
                }
            },
        )["data"]

    uuid = profile["attributes"]["uuid"]
    content = base64.b64decode(profile["attributes"]["profileContent"])
    # Where Xcode 16 and later look, and where earlier ones did.
    for folder in (
        "Library/Developer/Xcode/UserData/Provisioning Profiles",
        "Library/MobileDevice/Provisioning Profiles",
    ):
        path = pathlib.Path.home() / folder
        path.mkdir(parents=True, exist_ok=True)
        (path / f"{uuid}.provisionprofile").write_bytes(content)
    with tempfile.NamedTemporaryFile(suffix=".provisionprofile") as file:
        file.write(content)
        file.flush()
        decoded = subprocess.run(
            ["security", "cms", "-D", "-i", file.name], capture_output=True, check=True
        ).stdout
    entitlements = plistlib.loads(decoded)["Entitlements"]
    print(f"profile {NAME} {uuid}, entitlements:", file=sys.stderr)
    print(json.dumps(entitlements, indent=2, default=str), file=sys.stderr)
    print(uuid)


main()
