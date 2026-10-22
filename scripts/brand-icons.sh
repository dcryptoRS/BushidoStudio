#!/usr/bin/env bash
# Every size of Bushido Studio's icon from its master, assets/brand/
# bushido-studio.png (1024 px, transparent): the window's, the desktop's and
# the packages'. A vector master, assets/brand/bushido-studio.svg, is used
# instead when there is one.
set -euo pipefail
cd "$(dirname "$0")/.."
master=assets/brand/bushido-studio.png
if [ -f assets/brand/bushido-studio.svg ]; then
  ffmpeg -v error -y -i assets/brand/bushido-studio.svg -vf scale=1024:1024 "$master"
fi
python3 - "$master" <<'PY'
import sys
from PIL import Image
master = Image.open(sys.argv[1]).convert("RGBA")
for size in (16, 24, 32, 48, 64, 128, 256, 512):
    master.resize((size, size), Image.LANCZOS).save(f"assets/brand/bushido-studio-{size}.png")
PY
echo "icons made from $master"
