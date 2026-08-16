#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="ashell"
BUNDLE_ID="dev.ashell.app"
APP_DIR="$ROOT_DIR/target/release/${APP_NAME}.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
APP_VERSION="${APP_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -n 1)}"
APP_BUILD_VERSION="${APP_BUILD_VERSION:-$APP_VERSION}"

if [[ -z "$APP_VERSION" ]]; then
  echo "error: unable to read the package version from Cargo.toml" >&2
  exit 1
fi

cd "$ROOT_DIR"
cargo build --release

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cp "$ROOT_DIR/target/release/$APP_NAME" "$MACOS_DIR/$APP_NAME"

cp "$ROOT_DIR/assets/icons/ashell.icns" "$RESOURCES_DIR/ashell.icns"

cat > "$CONTENTS_DIR/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIconFile</key>
  <string>ashell.icns</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$APP_VERSION</string>
  <key>CFBundleVersion</key>
  <string>$APP_BUILD_VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
EOF

printf 'APPL????' > "$CONTENTS_DIR/PkgInfo"

if command -v codesign >/dev/null 2>&1; then
  # Important: do not pass an entitlements file here.
  # A sandboxed macOS app carries the `com.apple.security.app-sandbox` entitlement,
  # which would prevent the file access behavior this app needs.
  codesign --force --deep --sign "$SIGN_IDENTITY" "$APP_DIR" >/dev/null

  ENTITLEMENTS_XML="$(codesign -d --entitlements :- "$APP_DIR" 2>/dev/null || true)"
  if printf '%s' "$ENTITLEMENTS_XML" | grep -q "com.apple.security.app-sandbox"; then
    echo "error: app bundle is sandboxed; remove app sandbox entitlements before packaging" >&2
    exit 1
  fi
fi

echo "$APP_DIR"
