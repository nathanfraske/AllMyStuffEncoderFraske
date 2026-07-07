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
