#!/usr/bin/env bash
# Additional dependencies for the complete bench/ comparison suite.
set -euo pipefail

sudo apt-get install --yes --no-install-recommends \
  python3 time golang-go openjdk-21-jdk-headless \
  libjemalloc2 libtcmalloc-minimal4t64 libmimalloc3 \
  libicu-dev libssl-dev zlib1g

curl --fail --silent --show-error --location \
  https://dot.net/v1/dotnet-install.sh -o /tmp/solar-dotnet-install.sh
bash /tmp/solar-dotnet-install.sh --channel 10.0 --install-dir "$HOME/.dotnet" --no-path

curl --fail --silent --show-error --location https://install.julialang.org \
  | sh -s -- --yes --default-channel 1.13 --add-to-path=no

cat >> "$BASH_ENV" <<'ENV'
export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
export DOTNET_ROOT="$HOME/.dotnet"
export PATH="$HOME/.dotnet:$HOME/.juliaup/bin:$JAVA_HOME/bin:/usr/lib/go/bin:$PATH"
ENV

# LD_PRELOAD would only warn and fall back to glibc for a missing library.
# Require all allocator libraries so the report cannot silently mislabel runs.
for library in libjemalloc.so.2 libtcmalloc_minimal.so.4 libmimalloc.so.3; do
  test -r "/usr/lib/x86_64-linux-gnu/$library"
done
