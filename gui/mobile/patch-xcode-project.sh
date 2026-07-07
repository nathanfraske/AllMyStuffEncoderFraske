#!/bin/sh
# Run after every `tauri ios init`: turn Xcode's user-script sandboxing OFF
# in the generated project. Tauri's "Build Rust Code" phase writes the built
# staticlib into Externals/ and reads project.pbxproj — both outside the
# sandbox Xcode 16+ applies to script phases by default — so a sandboxed
# build dies with "Operation not permitted". `tauri ios init` regenerates
# the project with Apple's default, which is why this keeps coming back;
# handle both shapes (setting present as YES, or absent entirely).
set -e
PBX="$(cd "$(dirname "$0")" && pwd)/gen/apple/allmystuff-mobile.xcodeproj/project.pbxproj"
[ -f "$PBX" ] || { echo "no generated project at $PBX — run \`pnpm tauri ios init\` first" >&2; exit 1; }

perl -pi -e 's/ENABLE_USER_SCRIPT_SANDBOXING = YES/ENABLE_USER_SCRIPT_SANDBOXING = NO/g' "$PBX"
if ! grep -q "ENABLE_USER_SCRIPT_SANDBOXING" "$PBX"; then
  perl -pi -e 's/buildSettings = \{/buildSettings = {\n\t\t\t\tENABLE_USER_SCRIPT_SANDBOXING = NO;/g' "$PBX"
fi

echo "user-script sandboxing off ($(grep -c 'ENABLE_USER_SCRIPT_SANDBOXING = NO' "$PBX") setting block(s))"

# Stamp the brand mark into the generated asset catalog — init fills it
# with the default Tauri icon. The source is the FULL-BLEED iOS variant
# (icon-ios.png): iOS masks its own superellipse, so the desktop mark's
# rounded square + transparent corners would come back as a magenta
# tile floating on a white plate. `tauri icon` writes every iOS slot
# into gen/apple; the desktop-set byproduct lands in ./icons (ignored),
# never in ../src-tauri/icons.
DIR="$(cd "$(dirname "$0")" && pwd)"
if command -v pnpm >/dev/null 2>&1; then
  (cd "$DIR" && pnpm tauri icon ../src-tauri/icons/icon-ios.png) \
    && echo "app icon stamped into gen/apple" \
    || echo "icon stamp failed — run: cd $DIR && pnpm tauri icon ../src-tauri/icons/icon-ios.png" >&2
fi

# Drop the keyboard-accessory-bar suppressor into the generated Swift sources so
# Xcode compiles it into the app — its Objective-C `+load` self-installs the
# WKContentView swizzle, no wiring needed. With Xcode 16 synchronized groups the
# Sources folder is auto-included; on an older layout, add the file to the
# target once by hand. Deleting the file restores iOS's default prev/next/Done
# bar. Best-effort: `|| true` so a missing folder never fails the patch.
ACCESSORY_SRC="$DIR/ios/HideKeyboardAccessory.m"
ACCESSORY_DEST="$(ls -d "$DIR"/gen/apple/Sources/*/ 2>/dev/null | head -1)"
if [ -f "$ACCESSORY_SRC" ] && [ -n "$ACCESSORY_DEST" ]; then
  cp "$ACCESSORY_SRC" "$ACCESSORY_DEST" \
    && echo "keyboard accessory suppressor → $ACCESSORY_DEST" \
    || echo "accessory suppressor copy failed" >&2
else
  echo "accessory suppressor not copied (no gen/apple/Sources/* — init first)" >&2
fi
true
