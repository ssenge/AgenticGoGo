#!/bin/sh
# Renders the logo's composed assets from assets/src/*.html with headless Chrome (generated text is unreliable, so
# the mark is an image and everything with letters is laid out here). Usage: scripts/render_assets.sh
set -eu
cd "$(dirname "$0")/.."
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
shot() { # <html> <png> <width> <height>
  "$CHROME" --headless --disable-gpu --hide-scrollbars --default-background-color=00000000 \
    --screenshot="$2" --window-size="$3,$4" "file://$PWD/assets/src/$1" >/dev/null 2>&1
}
shot wordmark.html assets/logo-wordmark.png 1000 330
shot social.html assets/social.png 1280 640
big() { # <html> <png> <width> <height>: diagrams, at twice the size
  "$CHROME" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=2 --default-background-color=ffffffff \
    --screenshot="$2" --window-size="$3,$4" "file://$PWD/assets/src/$1" >/dev/null 2>&1
}
python3 assets/src/gen/features.py && python3 assets/src/gen/example.py
big intro.html assets/intro.png 1500 400
big features.html assets/features.png 1700 880
big example.html assets/example.png 1800 440
sips -Z 400 assets/src/mark.png --out assets/logo.png >/dev/null
sips -Z 800 assets/src/mark.png --out assets/logo@2x.png >/dev/null
echo "rendered: assets/intro.png assets/features.png assets/example.png assets/logo-wordmark.png assets/social.png assets/logo.png assets/logo@2x.png"
