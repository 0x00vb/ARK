#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/schema/v1" "$tmp/generated"
cargo run --quiet -p ark-protocol --example generate -- "$tmp"
diff -u "$root/crates/ark-protocol/schema/v1/protocol.schema.json" "$tmp/schema/v1/protocol.schema.json"
diff -u "$root/crates/ark-protocol/generated/ARKProtocol.swift" "$tmp/generated/ARKProtocol.swift"
