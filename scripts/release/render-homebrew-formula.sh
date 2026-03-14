#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <version> <crate-sha256> <output-path>" >&2
  exit 1
fi

version="$1"
crate_sha="$2"
output_path="$3"

mkdir -p "$(dirname "$output_path")"

cat >"$output_path" <<EOF
class Mcpsmith < Formula
  desc "Convert MCP servers into source-grounded skill packs with staged review and verify steps"
  homepage "https://crates.io/crates/mcpsmith"
  url "https://static.crates.io/crates/mcpsmith/mcpsmith-${version}.crate"
  version "${version}"
  sha256 "${crate_sha}"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "Usage: mcpsmith", shell_output("#{bin}/mcpsmith --help")
  end
end
EOF
