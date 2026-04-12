#!/usr/bin/env python3
"""
Generate RSA key pair, JWKS, and all test tokens needed by the OAuth2 Hurl tests.

Outputs written to $OUT_DIR (default: /tmp/oauth2-test/):
  private.pem           — RS256 signing key (kept for rotation step)
  jwks.json             — JWKS served by the mock HTTP server
  token_user.txt        — valid user JWT
  token_cc_admin.txt    — valid client-credentials JWT (role=admin)
  token_cc_tenant.txt   — valid client-credentials JWT (role=tenant-reader)
  token_expired.txt     — JWT with exp in the past
  token_bad_sig.txt     — JWT with last signature byte flipped
  token_wrong_aud.txt   — JWT with aud='wrong-audience'
  token_wrong_iss.txt   — JWT with iss='https://evil.example.com'
  token_dev.txt         — JWT with fake signature (for validation_mode:none test)
  vars.env              — shell-sourceable file with token variable exports
"""

import base64
import json
import os
import sys
import time

try:
    from cryptography.hazmat.primitives.asymmetric import rsa, padding
    from cryptography.hazmat.primitives import hashes, serialization
except ImportError:
    print("ERROR: cryptography package required — run: pip3 install cryptography", file=sys.stderr)
    sys.exit(1)

OUT = os.environ.get("OUT_DIR", "/tmp/oauth2-test")
ISSUER = os.environ.get("JWKS_ISSUER", "http://localhost:8888")
AUDIENCE = "esync-api"
KID = "test-key-1"

os.makedirs(OUT, exist_ok=True)


# ── Key generation ────────────────────────────────────────────────────────

key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
pub = key.public_key()

with open(f"{OUT}/private.pem", "wb") as f:
    f.write(key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ))


# ── JWKS ──────────────────────────────────────────────────────────────────

def int_to_b64url(n: int) -> str:
    length = (n.bit_length() + 7) // 8
    return base64.urlsafe_b64encode(n.to_bytes(length, "big")).rstrip(b"=").decode()


pub_numbers = pub.public_numbers()
jwks = {
    "keys": [{
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": KID,
        "n": int_to_b64url(pub_numbers.n),
        "e": int_to_b64url(pub_numbers.e),
    }]
}
with open(f"{OUT}/jwks.json", "w") as f:
    json.dump(jwks, f, indent=2)


# ── JWT helpers ───────────────────────────────────────────────────────────

def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def sign_jwt(claims: dict, kid: str = KID, signing_key=None) -> str:
    if signing_key is None:
        signing_key = key
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT", "kid": kid}).encode())
    payload = b64url(json.dumps(claims).encode())
    message = f"{header}.{payload}".encode()
    sig = signing_key.sign(message, padding.PKCS1v15(), hashes.SHA256())
    return f"{header}.{payload}.{b64url(sig)}"


now = int(time.time())


# ── Token: valid user ─────────────────────────────────────────────────────

user_token = sign_jwt({
    "iss": ISSUER,
    "aud": AUDIENCE,
    "sub": "user-001",
    "iat": now,
    "exp": now + 3600,
    "email": "alice@acme.com",
    "tenant_id": "acme",
    "preferred_username": "alice",
})
open(f"{OUT}/token_user.txt", "w").write(user_token)


# ── Token: admin client-credentials ──────────────────────────────────────

cc_admin_token = sign_jwt({
    "iss": ISSUER,
    "aud": AUDIENCE,
    "sub": "svc-admin",
    "iat": now,
    "exp": now + 3600,
    "client_id": "svc-admin",
    "roles": ["admin"],
    "gty": "client_credentials",
})
open(f"{OUT}/token_cc_admin.txt", "w").write(cc_admin_token)


# ── Token: tenant-reader client-credentials ───────────────────────────────

cc_tenant_token = sign_jwt({
    "iss": ISSUER,
    "aud": AUDIENCE,
    "sub": "svc-tenant-b",
    "iat": now,
    "exp": now + 3600,
    "client_id": "svc-tenant-b",
    "roles": ["tenant-reader"],
    "gty": "client_credentials",
})
open(f"{OUT}/token_cc_tenant.txt", "w").write(cc_tenant_token)


# ── Token: expired ────────────────────────────────────────────────────────

expired_token = sign_jwt({
    "iss": ISSUER,
    "aud": AUDIENCE,
    "sub": "user-old",
    "iat": now - 7200,
    "exp": now - 3600,   # already expired
})
open(f"{OUT}/token_expired.txt", "w").write(expired_token)


# ── Token: bad signature (flip last byte) ─────────────────────────────────

_parts = user_token.rsplit(".", 1)
_last = _parts[1]
_flipped = _last[:-1] + ("A" if _last[-1] != "A" else "B")
bad_sig_token = f"{_parts[0]}.{_flipped}"
open(f"{OUT}/token_bad_sig.txt", "w").write(bad_sig_token)


# ── Token: wrong audience ─────────────────────────────────────────────────

wrong_aud_token = sign_jwt({
    "iss": ISSUER,
    "aud": "completely-different-service",
    "sub": "user-x",
    "iat": now,
    "exp": now + 3600,
    "preferred_username": "user-x",
})
open(f"{OUT}/token_wrong_aud.txt", "w").write(wrong_aud_token)


# ── Token: wrong issuer ───────────────────────────────────────────────────

wrong_iss_token = sign_jwt({
    "iss": "https://evil.example.com",
    "aud": AUDIENCE,
    "sub": "user-evil",
    "iat": now,
    "exp": now + 3600,
    "preferred_username": "evil",
})
open(f"{OUT}/token_wrong_iss.txt", "w").write(wrong_iss_token)


# ── Token: dev / fake sig (for validation_mode:none) ─────────────────────

hdr = b64url(json.dumps({"alg": "RS256", "typ": "JWT", "kid": "dev-key"}).encode())
pay = b64url(json.dumps({
    "sub": "dev-user",
    "iat": now,
    "exp": now + 3600,
    "tenant_id": "dev-tenant",
    "email": "dev@test.com",
    "preferred_username": "dev-user",
}).encode())
fake_sig = b64url(b"this-is-not-a-real-signature")
dev_token = f"{hdr}.{pay}.{fake_sig}"
open(f"{OUT}/token_dev.txt", "w").write(dev_token)


# ── vars.env: sourceable shell exports ────────────────────────────────────

vars_env = f"""\
export USER_TOKEN="{user_token}"
export CC_ADMIN_TOKEN="{cc_admin_token}"
export CC_TENANT_TOKEN="{cc_tenant_token}"
export EXPIRED_TOKEN="{expired_token}"
export BAD_SIG_TOKEN="{bad_sig_token}"
export WRONG_AUD_TOKEN="{wrong_aud_token}"
export WRONG_ISS_TOKEN="{wrong_iss_token}"
export DEV_TOKEN="{dev_token}"
"""
open(f"{OUT}/vars.env", "w").write(vars_env)

print(f"Generated in {OUT}:")
for f in sorted(os.listdir(OUT)):
    size = os.path.getsize(f"{OUT}/{f}")
    print(f"  {f:<30} {size:>6} bytes")
