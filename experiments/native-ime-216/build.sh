#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
printf '%s\n' '33ac1a0325dcfefd566773928917b0251f54078d0f530852cbfe82aa0270a0d9  input-method-unstable-v2.xml' | sha256sum --check --status
mkdir -p build
wayland-scanner client-header input-method-unstable-v2.xml build/input-method-v2-client.h
wayland-scanner private-code input-method-unstable-v2.xml build/input-method-v2-protocol.c
read -r -a wayland_flags <<< "$(pkg-config --cflags --libs wayland-client)"
/usr/bin/cc -std=c11 -O2 -Wall -Wextra -Werror -Ibuild -o build/ime216 \
  ime216.c build/input-method-v2-protocol.c "${wayland_flags[@]}"
