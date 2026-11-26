#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

potrace="${POTRACE:-potrace}"
scratch="$(mktemp -d)"
trap 'rm -f "$scratch/outline.svg" "$scratch/fill.svg"; rmdir "$scratch"' EXIT
if [ -f ../Kahve.png ]; then cp ../Kahve.png assets/Kahve.png; fi
convert assets/Kahve.png -resize '450x450' -background none -gravity center -extent 512x512 PNG32:assets/icon.png

# libmpv 0.41 treats raw bitmap overlays as video-colored. Trace the two
# tones of the logo so the player can draw it on its SDR ASS layer instead.
convert assets/Kahve.png -alpha extract -threshold 50% -negate pbm:- |
    "$potrace" --svg --output "$scratch/outline.svg" -
convert assets/Kahve.png -background white -alpha remove -alpha off -colorspace gray -threshold 45% pbm:- |
    "$potrace" --svg --output "$scratch/fill.svg" -
{
    set -- $(identify -format '%w %h' assets/Kahve.png)
    printf '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %s %s">\n' "$1" "$2"
    sed -n '/^<g /,/^<\/g>/p' "$scratch/outline.svg" | sed 's/#000000/#c69b82/'
    sed -n '/^<g /,/^<\/g>/p' "$scratch/fill.svg" | sed 's/#000000/#713d1f/'
    printf '</svg>\n'
} > assets/Kahve-osd.svg
