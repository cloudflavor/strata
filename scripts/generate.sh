#!/usr/bin/env bash

set -e -o pipefail

LOG_LEVEL="${1:-debug}"

pushd crates/

cargo build --release

./target/release/nokturn-gen --log-level "$LOG_LEVEL" \
  generate \
  -c ~/projects/rust/skygen/open-api-specs/digitalocean/config.toml \
  -s ~/projects/rust/skygen/open-api-specs/digitalocean/digitalocean.yaml \
  -o ~/projects/rust/skygen/generated/digitalocean

popd
