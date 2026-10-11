#!/usr/bin/env bash
# The Linux tests that need an X server: hotkeys, the clipboard, the windows
# on screen and recording. They run on a private, invisible server (Xvfb,
# with openbox as its window manager), so the keys they press and the
# windows they open never reach the desktop you are using.
#
#   sudo apt install xvfb openbox gstreamer1.0-plugins-good gstreamer1.0-plugins-ugly gstreamer1.0-libav
#   scripts/linux-x11-tests.sh
#
# With PipeWire running, recording a microphone is tested too: on a silent
# output of the script's own (pw-loopback), gone when the script ends, so
# nothing is heard. The report is also kept in tmp/linux-x11-tests.log.
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
loopback=
cleanup() {
  [ -n "$loopback" ] && kill "$loopback" 2>/dev/null || true
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

# A silent output that plays back into a virtual microphone: the sound
# test plays a tone into it and records its monitor.
sound_tests=(--skip records_a_microphone)
if command -v pw-loopback >/dev/null && pw-cli info 0 >/dev/null 2>&1; then
  pw-loopback \
    --capture-props="media.class=Audio/Sink node.name=shuttercrab-test-sink audio.position=[FL,FR]" \
    --playback-props="media.class=Audio/Source node.name=shuttercrab-test-mic audio.position=[FL,FR]" \
    >"$work/loopback.log" 2>&1 &
  loopback=$!
  sleep 0.5
  export SHUTTERCRAB_TEST_SOUND_SINK=shuttercrab-test-sink
  sound_tests=()
fi

echo "Private X server on $DISPLAY"
{
  [ -z "$loopback" ] && echo "No PipeWire: recording a microphone is not tested"
  cargo test -p shuttercrab-platform --locked --test x11 -- --ignored --test-threads=1
  cargo test -p shuttercrab-capture --locked --test x11_record -- --ignored "${sound_tests[@]}"
} 2>&1 | tee tmp/linux-x11-tests.log
