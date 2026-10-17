#!/bin/sh
# System libraries GPUI needs on Debian/Ubuntu (from gpui-kit's script/install-linux.sh).
set -e
sudo apt-get update
sudo apt-get install -y gcc g++ clang libfontconfig-dev libwayland-dev \
  libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libasound2-dev libvulkan1
