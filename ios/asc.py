#!/usr/bin/env python3
"""Minimal App Store Connect API client.

Enough to check what exists and register a bundle ID before the first
TestFlight upload. Deliberately dependency-free: it signs the ES256 JWT with
`openssl` and a hand-rolled DER->raw conversion rather than pulling in PyJWT
or `cryptography`, neither of which is present on a stock macOS Python.

Usage:
    ASC_KEY_ID=... ASC_ISSUER_ID=... ./ios/asc.py whoami
    ASC_KEY_ID=... ASC_ISSUER_ID=... ./ios/asc.py apps
    ASC_KEY_ID=... ASC_ISSUER_ID=... ./ios/asc.py register-bundle \\
        dev.fantasy-agent.ios "Fantasy Agent"

Credentials come from the environment and the key from
~/.appstoreconnect/private_keys/AuthKey_<KEY_ID>.p8 (override with
ASC_KEY_PATH). Nothing is written to disk.
"""

import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

API = "https://api.appstoreconnect.apple.com"


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def der_to_raw(der: bytes) -> bytes:
    """Convert an ECDSA DER signature to the raw r||s JWS wants.

    DER is 30 <len> 02 <rlen> <r> 02 <slen> <s>, with r and s as signed
    big-endian integers — so they may carry a leading zero byte that has to
    come off, and may be shorter than 32 bytes and need padding back on.
    """
    if not der or der[0] != 0x30:
        raise ValueError("not a DER sequence")
    i = 2
    # A length over 127 is encoded as 0x8N followed by N length bytes.
    if der[1] & 0x80:
        i = 2 + (der[1] & 0x7F)

    def read_int(pos):
        if der[pos] != 0x02:
            raise ValueError("expected a DER integer")
        length = der[pos + 1]
        val = der[pos + 2 : pos + 2 + length]
        return val.lstrip(b"\x00").rjust(32, b"\x00"), pos + 2 + length

    r, i = read_int(i)
    s, _ = read_int(i)
    return r + s


def token(key_id: str, issuer_id: str, key_path: str) -> str:
    header = {"alg": "ES256", "kid": key_id, "typ": "JWT"}
    now = int(time.time())
    payload = {
        "iss": issuer_id,
        "iat": now,
        # Apple rejects anything beyond 20 minutes.
        "exp": now + 1200,
        "aud": "appstoreconnect-v1",
    }
    signing_input = "{}.{}".format(
        b64url(json.dumps(header, separators=(",", ":")).encode()),
        b64url(json.dumps(payload, separators=(",", ":")).encode()),
    ).encode()

    proc = subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", key_path],
        input=signing_input,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise SystemExit(f"openssl could not sign with {key_path}:\n{proc.stderr.decode()}")
    return f"{signing_input.decode()}.{b64url(der_to_raw(proc.stdout))}"


def request(method: str, path: str, jwt: str, body=None):
    req = urllib.request.Request(
        path if path.startswith("http") else API + path,
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": f"Bearer {jwt}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, {"raw": raw.decode(errors="replace")}


def errors_of(payload) -> str:
    out = []
    for err in payload.get("errors", []):
        line = f"  {err.get('title', '?')}: {err.get('detail', '')}"
        out.append(line.rstrip())
    return "\n".join(out) or f"  {payload}"


def main() -> int:
    key_id = os.environ.get("ASC_KEY_ID")
    issuer = os.environ.get("ASC_ISSUER_ID")
    if not key_id or not issuer:
        print(
            "Set ASC_KEY_ID and ASC_ISSUER_ID (App Store Connect > Users and\n"
            "Access > Integrations > App Store Connect API).",
            file=sys.stderr,
        )
        return 2
    key_path = os.environ.get(
        "ASC_KEY_PATH",
        os.path.expanduser(f"~/.appstoreconnect/private_keys/AuthKey_{key_id}.p8"),
    )
    if not os.path.exists(key_path):
        print(f"No API key at {key_path}", file=sys.stderr)
        return 2

    jwt = token(key_id, issuer, key_path)
    cmd = sys.argv[1] if len(sys.argv) > 1 else "whoami"

    if cmd == "whoami":
        # Cheapest authenticated call there is; proves the credentials work.
        status, body = request("GET", "/v1/apps?limit=1", jwt)
        if status == 200:
            print(f"Authenticated. Key {key_id}, issuer {issuer}.")
            return 0
        print(f"Authentication failed ({status}):\n{errors_of(body)}", file=sys.stderr)
        return 1

    if cmd == "apps":
        status, body = request("GET", "/v1/apps?limit=200", jwt)
        if status != 200:
            print(f"Could not list apps ({status}):\n{errors_of(body)}", file=sys.stderr)
            return 1
        rows = body.get("data", [])
        if not rows:
            print("No apps in this account.")
        for a in rows:
            at = a["attributes"]
            print(f"  {at.get('bundleId','?'):40} {at.get('name','?')}  (id {a['id']})")
        return 0

    if cmd == "bundles":
        status, body = request("GET", "/v1/bundleIds?limit=200", jwt)
        if status != 200:
            print(f"Could not list bundle ids ({status}):\n{errors_of(body)}", file=sys.stderr)
            return 1
        for b in body.get("data", []):
            at = b["attributes"]
            print(f"  {at.get('identifier','?'):40} {at.get('name','?')}")
        return 0

    if cmd == "register-bundle":
        if len(sys.argv) < 4:
            print("usage: asc.py register-bundle <identifier> <name>", file=sys.stderr)
            return 2
        identifier, name = sys.argv[2], sys.argv[3]
        status, body = request(
            "POST",
            "/v1/bundleIds",
            jwt,
            {
                "data": {
                    "type": "bundleIds",
                    "attributes": {
                        "identifier": identifier,
                        "name": name,
                        "platform": "IOS",
                    },
                }
            },
        )
        if status in (200, 201):
            print(f"Registered bundle id {identifier}.")
            return 0
        # Already existing is a success for our purposes.
        if any("already exists" in (e.get("detail") or "").lower()
               for e in body.get("errors", [])):
            print(f"Bundle id {identifier} already registered.")
            return 0
        print(f"Could not register ({status}):\n{errors_of(body)}", file=sys.stderr)
        return 1

    print(f"unknown command: {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
