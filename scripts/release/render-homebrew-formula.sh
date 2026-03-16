#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <version> <archive-sha256> <output-path>" >&2
  exit 1
fi

version="$1"
archive_sha="$2"
output_path="$3"

mkdir -p "$(dirname "$output_path")"

cat >"$output_path" <<EOF
class Mcpsmith < Formula
  desc "Convert MCP servers into source-grounded skill packs with staged review and verify steps"
  homepage "https://github.com/nclandrei/mcpsmith"
  url "https://github.com/nclandrei/mcpsmith/archive/refs/tags/v${version}.tar.gz"
  version "${version}"
  sha256 "${archive_sha}"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match "Usage: mcpsmith", shell_output("#{bin}/mcpsmith --help")
  end
end
EOF
