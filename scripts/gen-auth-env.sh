#!/usr/bin/env bash
# Generates a JWT secret and one minted token per identity, writing dotenv
# vars a docker-compose.auth.yml overlay consumes via
# `docker compose --env-file`. This is the "easy button" for turning on
# gRPC JWT auth (.claude/plans/grpc-security/PRD.md) in any example stack
# without hand-editing YAML or minting tokens one at a time.
#
# A stack's own Djinn service(s) never need a token minted here — once
# COORDIN8_JWT_SECRET is set, each service self-mints its own tokens for
# its internal Djinn-to-Djinn calls (Decision 8). Only pass the identities
# of external application clients (greeter, auction-service, etc.).
#
# Usage: scripts/gen-auth-env.sh <output-env-file> [identity...]
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 <output-env-file> [identity...]" >&2
  exit 1
fi

out="$1"
shift

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cli="${COORDIN8_CLI:-}"
if [ -z "$cli" ]; then
  cli_dir="$(mktemp -d)"
  cli="$cli_dir/coordin8"
  (cd "$repo_root/cli" && go build -o "$cli" ./cmd/coordin8)
fi

secret="${COORDIN8_JWT_SECRET:-$(openssl rand -hex 32)}"

# 720h (30-day) TTL, no jti/revocation: an accepted v1 trade-off of the
# static-token model (Decision 2, .claude/plans/grpc-security/PRD.md) —
# there is no live issuance/revocation service, so the only remedy for a
# leaked token is rotating the shared secret, which invalidates every
# other token at the same time. Fine for dev/example stacks; a real
# deployment should mint shorter-lived tokens and rotate on its own
# schedule instead of relying on this script's default.
: > "$out"
chmod 600 "$out"

{
  echo "COORDIN8_JWT_SECRET=$secret"
  for identity in "$@"; do
    # Pass the secret via env, not --secret — an argv value is visible to
    # any other user on the box via `ps`, and the CLI already reads
    # $COORDIN8_JWT_SECRET on its own (same as every other Coordin8 knob).
    token=$(COORDIN8_JWT_SECRET="$secret" "$cli" auth mint-token --sub "$identity" --ttl 720h)
    var_name="COORDIN8_TOKEN_$(echo "$identity" | tr '[:lower:]-' '[:upper:]_')"
    echo "$var_name=$token"
  done
} >> "$out"

echo "wrote $out (1 secret + $# token(s))" >&2
