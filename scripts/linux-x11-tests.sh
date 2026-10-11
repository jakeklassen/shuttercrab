#!/usr/bin/env bash
# The Linux tests that need an X server: hotkeys, the clipboard, the windows
# on screen and recording. They run on a private, invisible server (Xvfb,
# with openbox as its window manager), so the keys they press and the
# windows they open never reach the desktop you are using.
#
#   sudo apt install xvfb openbox gstreamer1.0-plugins-good gstreamer1.0-plugins-ugly
#   scripts/linux-x11-tests.sh
#
# The report is also kept in tmp/linux-x11-tests.log.
set -euo pipefail
cd "$(dirname "$0")/.."

for tool in Xvfb openbox cargo; do
  if ! command -v "$tool" >/dev/null; then
    echo "$tool is missing (sudo apt install xvfb openbox; cargo from rustup or mise)" >&2
    exit 1
  fi
done

mkdir -p tmp
work=$(mktemp -d tmp/x11-tests.XXXXXX)
xvfb=
wm=
cleanup() {
  [ -n "$wm" ] && kill "$wm" 2>/dev/null || true
  [ -n "$xvfb" ] && kill "$xvfb" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# Xvfb picks a free display number and writes it to its stdout.
Xvfb -displayfd 1 -screen 0 1920x1080x24 -nolisten tcp >"$work/display" 2>"$work/xvfb.log" &
xvfb=$!
for _ in $(seq 100); do
  [ -s "$work/display" ] && break
  sleep 0.05
done
if [ ! -s "$work/display" ]; then
  echo "Xvfb did not start:" >&2
  cat "$work/xvfb.log" >&2
  exit 1
fi
export DISPLAY=":$(head -n1 "$work/display")"
unset WAYLAND_DISPLAY
export SHUTTERCRAB_TEST_X11=1

openbox >"$work/openbox.log" 2>&1 &
wm=$!

echo "Private X server on $DISPLAY"
{
  cargo test -p shuttercrab-platform --locked --test x11 -- --ignored --test-threads=1
  cargo test -p shuttercrab-capture --locked --test x11_record -- --ignored
} 2>&1 | tee tmp/linux-x11-tests.log
