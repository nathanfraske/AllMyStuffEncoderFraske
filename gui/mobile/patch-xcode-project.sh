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
# with the default Tauri icon. `tauri icon` regenerates every iOS slot
# (and the desktop set, byte-identical) from the 512x512 source.
DIR="$(cd "$(dirname "$0")" && pwd)"
if command -v pnpm >/dev/null 2>&1; then
  (cd "$DIR" && pnpm tauri icon ../src-tauri/icons/icon.png) \
    && echo "app icon stamped into gen/apple" \
    || echo "icon stamp failed — run: cd $DIR && pnpm tauri icon ../src-tauri/icons/icon.png" >&2
fi
