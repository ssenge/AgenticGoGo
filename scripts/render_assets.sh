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
sips -Z 400 assets/src/mark.png --out assets/logo.png >/dev/null
sips -Z 800 assets/src/mark.png --out assets/logo@2x.png >/dev/null
echo "rendered: assets/logo-wordmark.png assets/social.png assets/logo.png assets/logo@2x.png"
