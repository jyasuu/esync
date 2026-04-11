#!/usr/bin/env python3
"""
Rotate the JWKS to a new key (kid=test-key-2) and generate a token signed
with it.  Called from the workflow after the core test suite passes, so
the old user_token (kid=test-key-1) becomes invalid.

Writes:
  $OUT_DIR/jwks.json          — updated to contain ONLY kid=test-key-2
  $OUT_DIR/token_rotated.txt  — token signed with the new key
  $OUT_DIR/vars_rotated.env   — ROTATED_TOKEN export
"""

import base64
import json
import os
import time

from cryptography.hazmat.primitives.asymmetric import rsa, padding
from cryptography.hazmat.primitives import hashes, serialization

OUT = os.environ.get("OUT_DIR", "/tmp/oauth2-test")
ISSUER = os.environ.get("JWKS_ISSUER", "http://localhost:8888")
AUDIENCE = "esync-api"
NEW_KID = "test-key-2"

os.makedirs(OUT, exist_ok=True)

# Generate fresh key pair for the new kid
key2 = rsa.generate_private_key(public_exponent=65537, key_size=2048)
pub2 = key2.public_key()
pub2_numbers = pub2.public_numbers()


def int_to_b64url(n: int) -> str:
    length = (n.bit_length() + 7) // 8
    return base64.urlsafe_b64encode(n.to_bytes(length, "big")).rstrip(b"=").decode()


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


# Replace JWKS — old kid=test-key-1 is gone
new_jwks = {
    "keys": [{
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": NEW_KID,
        "n": int_to_b64url(pub2_numbers.n),
        "e": int_to_b64url(pub2_numbers.e),
    }]
}
with open(f"{OUT}/jwks.json", "w") as f:
    json.dump(new_jwks, f, indent=2)

# Token signed with the new key
now = int(time.time())
hdr = b64url(json.dumps({"alg": "RS256", "typ": "JWT", "kid": NEW_KID}).encode())
pay = b64url(json.dumps({
    "iss": ISSUER,
    "aud": AUDIENCE,
    "sub": "user-rotated",
    "iat": now,
    "exp": now + 3600,
    "email": "rotated@acme.com",
    "tenant_id": "acme",
    "preferred_username": "rotated",
}).encode())
message = f"{hdr}.{pay}".encode()
sig = key2.sign(message, padding.PKCS1v15(), hashes.SHA256())
rotated_token = f"{hdr}.{pay}.{b64url(sig)}"

open(f"{OUT}/token_rotated.txt", "w").write(rotated_token)
open(f"{OUT}/vars_rotated.env", "w").write(f'export ROTATED_TOKEN="{rotated_token}"\n')

print(f"JWKS rotated to {NEW_KID}")
print(f"Rotated token written to {OUT}/token_rotated.txt")
