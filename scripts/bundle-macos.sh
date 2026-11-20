#!/bin/bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Build the macOS app on macOS." >&2
  exit 1
fi

repo="$(cd "$(dirname "$0")/.." && pwd)"
target="$repo/target"
case "${1:-}" in
  --run)
    shift
    executable="$1"
    shift
    # Cargo also uses the runner for test harnesses and examples.
    if [[ "${executable##*/}" != "zotero-cloud-bridge" ]]; then
      exec "$executable" "$@"
    fi
    run_app=true
    ;;
  "")
    cargo build --release --locked --manifest-path "$repo/Cargo.toml" --target-dir "$target"
    executable="$target/release/zotero-cloud-bridge"
    run_app=false
    ;;
  *)
    echo "Usage: scripts/bundle-macos.sh" >&2
    exit 1
    ;;
esac

version=v1.76.0
case "$(lipo -archs "$executable")" in
  arm64)
    arch=arm64
    checksum=e72dc90b70f4947a7b72b61bde4a4fbba5777b2874fb448951ffdde67ebd0ab7
    ;;
  x86_64)
    arch=amd64
    checksum=4f5835733d52b818340f97b1fc0348a70461534d123363a183ca2b660219595a
    ;;
  *)
    echo "Unsupported macOS architecture." >&2
    exit 1
    ;;
esac

cache="$target/rclone"
mkdir -p "$cache"
stage="$(mktemp -d "$target/.bundle.XXXXXXXX")"
trap 'rm -rf "$stage"' EXIT
release="rclone-$version-osx-$arch"
archive="$cache/$release.zip"

verify_archive() {
  printf '%s  %s\n' "$checksum" "$1" | shasum -a 256 --check --status
}

if [[ ! -f "$archive" ]] || ! verify_archive "$archive"; then
  download="$stage/$release.zip"
  printf 'Downloading rclone %s (%s)…\n' "$version" "$arch" >&2
  curl --fail --silent --show-error --location --retry 2 -o "$download" "https://downloads.rclone.org/$version/$release.zip"
  if ! verify_archive "$download"; then
    echo "The rclone download failed checksum verification." >&2
    exit 1
  fi
  mv -f "$download" "$archive"
fi

if "$run_app"; then
  bundle="$stage/Zotero Cloud Bridge.app"
else
  bundle="$target/Zotero Cloud Bridge.app"
fi
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
launcher="$bundle/Contents/MacOS/zotero-cloud-bridge"
install -m 755 "$executable" "$launcher"
unzip -p "$archive" "$release/rclone" > "$stage/rclone"
install -m 755 "$stage/rclone" "$bundle/Contents/Resources/rclone"
install -m 644 "$repo/assets/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>zotero-cloud-bridge</string>
  <key>CFBundleIdentifier</key>
  <string>com.truonghaidang.zotero-cloud-bridge</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon.icns</string>
  <key>CFBundleName</key>
  <string>Zotero Cloud Bridge</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>LSUIElement</key>
  <true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$bundle/Contents/Resources/rclone"
codesign --force --sign - "$bundle"

if "$run_app"; then
  "$launcher" "$@" &
  bridge_pid=$!
  stop_bridge() {
    kill -TERM "$bridge_pid" 2>/dev/null || true
    wait "$bridge_pid" || true
    exit "$1"
  }
  trap 'stop_bridge 130' INT
  trap 'stop_bridge 143' TERM
  wait "$bridge_pid"
else
  # Notify Finder when replacing the contents of an existing app bundle.
  touch "$bundle"
  printf '%s\n' "$bundle"
fi
