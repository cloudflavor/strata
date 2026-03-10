#!/usr/bin/env bash

set -e -o pipefail

USAGE="Usage: $0 <provider> [log-level]\nProviders: do, cf"

if [[ $# -lt 1 ]]; then
  echo "$USAGE" >&2
  exit 1
fi

PROVIDER="$1"
LOG_LEVEL="${2:-debug}"

case "$PROVIDER" in
  do)
    CONFIG="open-api-specs/digitalocean/config.toml"
    SCHEMA="open-api-specs/digitalocean/digitalocean.yaml"
    OUTPUT="generated/digitalocean"
    ;;
  cf)
    CONFIG="open-api-specs/cloudflare/config.toml"
    SCHEMA="open-api-specs/cloudflare/cloudflare.json"
    OUTPUT="generated/cloudflare"
    ;;
  *)
    echo "$USAGE" >&2
    exit 1
    ;;
esac

pushd crates/

cargo build --release

./target/release/nokturn-gen --log-level "$LOG_LEVEL" \
  generate \
  -c "$PWD/../$CONFIG" \
  -s "$PWD/../$SCHEMA" \
  -o "$PWD/../$OUTPUT"

popd
