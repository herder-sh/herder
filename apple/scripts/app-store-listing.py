#!/usr/bin/env python3
"""Uploads the iPhone app's App Store listing from apple/listing/listing.toml through the App
Store Connect API: the en-US copy, the categories, the age rating, the App Review notes and the
screenshots that listing-screenshots.py framed. It never submits anything for review.

usage: apple/scripts/app-store-listing.py upload <frames>   set it all, then show it
       apple/scripts/app-store-listing.py show              read the listing back
       apple/scripts/app-store-listing.py get <path>        print an API resource as JSON

Needs ASC_KEY_ID, ASC_ISSUER_ID and ASC_PRIVATE_KEY (the .p8's contents) in the environment,
as `herder secret run apple-herder/ASC_KEY_ID apple-herder/ASC_ISSUER_ID
apple-herder/ASC_PRIVATE_KEY -- apple/scripts/app-store-listing.py ...` puts them there, and
openssl. The key needs the App Manager role.
"""
import base64
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
API = "https://api.appstoreconnect.apple.com"
LOCALE = "en-US"
# The 6.9" iPhone's set, which takes its 1320×2868 screenshots.
DISPLAY = "APP_IPHONE_67"
# The version and app info states whose metadata can still change.
EDITABLE = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED", "METADATA_REJECTED"}


def b64(data: bytes) -> bytes:
    return base64.urlsafe_b64encode(data).rstrip(b"=")


def token() -> str:
    """A 10-minute ES256 JWT for the API key; the key never leaves a 0600 temp file."""
    # The issuer ID is stored with a trailing newline.
    kid, iss = os.environ["ASC_KEY_ID"].strip(), os.environ["ASC_ISSUER_ID"].strip()
    pem = os.environ["ASC_PRIVATE_KEY"].strip() + "\n"
    now = int(time.time())
    head = b64(json.dumps({"alg": "ES256", "kid": kid, "typ": "JWT"}).encode())
    body = b64(json.dumps({"iss": iss, "iat": now, "exp": now + 600, "aud": "appstoreconnect-v1"}).encode())
    message = head + b"." + body
    with tempfile.TemporaryDirectory() as dir:
        key = os.path.join(dir, "key.p8")
        fd = os.open(key, os.O_WRONLY | os.O_CREAT, 0o600)
        os.write(fd, pem.encode())
        os.close(fd)
        der = subprocess.run(["openssl", "dgst", "-sha256", "-sign", key], input=message,
                             capture_output=True, check=True).stdout
    # openssl signs in DER; a JWT carries r and s as two raw 32-byte integers.
    parts, i = [], 2
    for _ in range(2):
        length = der[i + 1]
        parts.append(der[i + 2:i + 2 + length].lstrip(b"\0").rjust(32, b"\0"))
        i += 2 + length
    return (message + b"." + b64(parts[0] + parts[1])).decode()


class Api:
    def __init__(self) -> None:
        self.jwt, self.minted = token(), time.time()

    def call(self, method: str, path: str, body: dict | None = None) -> dict | None:
        if time.time() - self.minted > 500:
            self.jwt, self.minted = token(), time.time()
        request = urllib.request.Request(
            API + path, method=method, data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Bearer " + self.jwt, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read()
        except urllib.error.HTTPError as error:
            sys.exit(f"{method} {path}: HTTP {error.code}\n{error.read().decode()[:2000]}")
        return json.loads(raw) if raw else None

    def get(self, path: str) -> dict:
        return self.call("GET", path)

    def patch(self, kind: str, id: str, attributes: dict | None = None, relationships: dict | None = None) -> dict:
        data = {"type": kind, "id": id}
        if attributes:
            data["attributes"] = attributes
        if relationships:
            data["relationships"] = relationships
        return self.call("PATCH", f"/v1/{kind}/{id}", {"data": data})["data"]

    def post(self, kind: str, attributes: dict, relationships: dict) -> dict:
        body = {"data": {"type": kind, "attributes": attributes, "relationships": relationships}}
        return self.call("POST", f"/v1/{kind}", body)["data"]


def to(kind: str, id: str) -> dict:
    """A to-one relationship."""
    return {"data": {"type": kind, "id": id}}


def localized(api: Api, path: str) -> dict | None:
    return next((l for l in api.get(path)["data"] if l["attributes"]["locale"] == LOCALE), None)


def editable(items: list[dict], state: str) -> dict:
    found = [item for item in items if item["attributes"].get(state) in EDITABLE]
    if not found:
        sys.exit(f"nothing editable: {[(i['id'], i['attributes'].get(state)) for i in items]}")
    return found[0]


class Listing:
    """The app's editable version and app info, as the API names them."""

    def __init__(self, api: Api, listing: dict) -> None:
        self.api, self.listing = api, listing
        app, platform = listing["app"], listing["platform"]
        versions = api.get(f"/v1/apps/{app}/appStoreVersions?filter[platform]={platform}")["data"]
        self.version = editable(versions, "appStoreState")
        self.info = editable(api.get(f"/v1/apps/{app}/appInfos")["data"], "appStoreState")

    def version_localization(self) -> dict:
        path = f"/v1/appStoreVersions/{self.version['id']}/appStoreVersionLocalizations"
        found = localized(self.api, path)
        if found:
            return found
        return self.api.post("appStoreVersionLocalizations", {"locale": LOCALE},
                             {"appStoreVersion": to("appStoreVersions", self.version["id"])})

    def copy(self) -> None:
        copy = self.listing[LOCALE]
        self.api.patch("appStoreVersionLocalizations", self.version_localization()["id"], {
            "description": copy["description"],
            "keywords": copy["keywords"],
            "promotionalText": copy["promotional_text"],
            "supportUrl": copy["support_url"],
            "marketingUrl": copy["marketing_url"],
        })
        info = localized(self.api, f"/v1/appInfos/{self.info['id']}/appInfoLocalizations")
        if info is None:
            sys.exit(f"the app info has no {LOCALE} localization")
        self.api.patch("appInfoLocalizations", info["id"], {
            "name": copy["name"],
            "subtitle": copy["subtitle"],
            "privacyPolicyUrl": copy["privacy_policy_url"],
        })

    def categories(self) -> None:
        self.api.patch("appInfos", self.info["id"], relationships={
            "primaryCategory": to("appCategories", self.listing["primary_category"]),
            "secondaryCategory": to("appCategories", self.listing["secondary_category"]),
        })

    def age_rating(self) -> None:
        declaration = self.api.get(f"/v1/appInfos/{self.info['id']}/ageRatingDeclaration")["data"]
        self.api.patch("ageRatingDeclarations", declaration["id"], self.listing["age_rating"])

    def review(self) -> None:
        """The App Review notes; the contact stays for the owner to fill in."""
        detail = self.api.get(f"/v1/appStoreVersions/{self.version['id']}/appStoreReviewDetail")["data"]
        attributes = {"notes": self.listing["review"]["notes"], "demoAccountRequired": False}
        if detail:
            # A change needs the contact filled in, which a new detail does not.
            if detail["attributes"]["notes"] != attributes["notes"]:
                self.api.patch("appStoreReviewDetails", detail["id"], attributes)
        else:
            self.api.post("appStoreReviewDetails", attributes,
                          {"appStoreVersion": to("appStoreVersions", self.version["id"])})

    def screenshots(self, frames: Path) -> None:
        """Replaces the 6.9" set's screenshots with `frames`, in the order of their names."""
        files = sorted(frames.glob("*.png"))
        if not files:
            sys.exit(f"no frames in {frames}")
        localization = self.version_localization()["id"]
        sets = self.api.get(f"/v1/appStoreVersionLocalizations/{localization}/appScreenshotSets")["data"]
        set = next((s for s in sets if s["attributes"]["screenshotDisplayType"] == DISPLAY), None)
        if set is None:
            set = self.api.post("appScreenshotSets", {"screenshotDisplayType": DISPLAY},
                                {"appStoreVersionLocalization": to("appStoreVersionLocalizations", localization)})
        for old in self.api.get(f"/v1/appScreenshotSets/{set['id']}/appScreenshots")["data"]:
            self.api.call("DELETE", f"/v1/appScreenshots/{old['id']}")
        ids = {self.upload(set["id"], file): file.name for file in files}
        self.api.call("PATCH", f"/v1/appScreenshotSets/{set['id']}/relationships/appScreenshots",
                      {"data": [{"type": "appScreenshots", "id": id} for id in ids]})
        # Apple processes each after its commit, which can take minutes; a frame it cannot take
        # fails here.
        pending = dict(ids)
        for _ in range(120):
            for id, name in list(pending.items()):
                state = self.api.get(f"/v1/appScreenshots/{id}")["data"]["attributes"]["assetDeliveryState"]
                if state["state"] == "FAILED":
                    sys.exit(f"{name} failed: {state.get('errors')}")
                if state["state"] == "COMPLETE":
                    print(f"processed {name}")
                    del pending[id]
            if not pending:
                return
            time.sleep(5)
        sys.exit(f"still processing: {', '.join(pending.values())}")

    def upload(self, set: str, file: Path) -> str:
        """Reserves a screenshot, uploads its parts where the reservation says, and commits it."""
        data = file.read_bytes()
        shot = self.api.post("appScreenshots", {"fileName": file.name, "fileSize": len(data)},
                             {"appScreenshotSet": to("appScreenshotSets", set)})
        for part in shot["attributes"]["uploadOperations"]:
            chunk = data[part["offset"]:part["offset"] + part["length"]]
            headers = {h["name"]: h["value"] for h in part["requestHeaders"]}
            request = urllib.request.Request(part["url"], data=chunk, method=part["method"], headers=headers)
            urllib.request.urlopen(request, timeout=120).close()
        self.api.patch("appScreenshots", shot["id"], {
            "uploaded": True, "sourceFileChecksum": hashlib.md5(data).hexdigest()})
        print(f"uploaded {file.name}")
        return shot["id"]

    def show(self) -> None:
        version, info = self.version, self.info
        print(f"version {version['attributes']['versionString']}: {version['attributes']['appStoreState']}")
        localization = self.version_localization()
        for key in ["description", "keywords", "promotionalText", "supportUrl", "marketingUrl"]:
            print(f"{key}: {localization['attributes'][key]}")
        names = localized(self.api, f"/v1/appInfos/{info['id']}/appInfoLocalizations")["attributes"]
        for key in ["name", "subtitle", "privacyPolicyUrl"]:
            print(f"{key}: {names[key]}")
        relations = self.api.get(f"/v1/appInfos/{info['id']}?include=primaryCategory,secondaryCategory")
        for key in ["primaryCategory", "secondaryCategory"]:
            print(f"{key}: {(relations['data']['relationships'][key].get('data') or {}).get('id')}")
        rating = self.api.get(f"/v1/appInfos/{info['id']}/ageRatingDeclaration")["data"]["attributes"]
        print("ageRatingDeclaration:", ", ".join(f"{k}={v}" for k, v in rating.items() if v is not None))
        print("appStoreAgeRating:", self.api.get(f"/v1/appInfos/{info['id']}")["data"]["attributes"]["appStoreAgeRating"])
        detail = self.api.get(f"/v1/appStoreVersions/{version['id']}/appStoreReviewDetail")["data"]
        print("review notes:", detail and detail["attributes"]["notes"])
        sets = self.api.get(f"/v1/appStoreVersionLocalizations/{localization['id']}/appScreenshotSets")["data"]
        for set in sets:
            shots = self.api.get(f"/v1/appScreenshotSets/{set['id']}/appScreenshots")["data"]
            print(f"{set['attributes']['screenshotDisplayType']}:", ", ".join(
                f"{s['attributes']['fileName']} ({s['attributes']['assetDeliveryState']['state']})" for s in shots))


def main() -> None:
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "get" and len(sys.argv) == 3:
        print(json.dumps(Api().get(sys.argv[2]), indent=2))
        return
    if command not in ("upload", "show") or len(sys.argv) != (3 if command == "upload" else 2):
        sys.exit(__doc__)
    listing = Listing(Api(), tomllib.loads((ROOT / "apple/listing/listing.toml").read_text()))
    if command == "upload":
        listing.copy()
        listing.categories()
        listing.age_rating()
        listing.review()
        listing.screenshots(Path(sys.argv[2]))
    listing.show()


if __name__ == "__main__":
    main()
