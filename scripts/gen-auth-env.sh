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

{
  echo "COORDIN8_JWT_SECRET=$secret"
  for identity in "$@"; do
    token=$("$cli" auth mint-token --secret "$secret" --sub "$identity" --ttl 720h)
    var_name="COORDIN8_TOKEN_$(echo "$identity" | tr '[:lower:]-' '[:upper:]_')"
    echo "$var_name=$token"
  done
} > "$out"

echo "wrote $out (1 secret + $# token(s))" >&2
