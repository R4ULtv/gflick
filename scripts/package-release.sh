#!/usr/bin/env bash
set -euo pipefail

version=""
target=""
architecture=""
binary_dir=""
output_dir=""
settings_app_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version=${2-}; shift 2 ;;
    --target) target=${2-}; shift 2 ;;
    --arch) architecture=${2-}; shift 2 ;;
    --binary-dir) binary_dir=${2-}; shift 2 ;;
    --output-dir) output_dir=${2-}; shift 2 ;;
    --settings-app-dir) settings_app_dir=${2-}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "version must be an unprefixed semantic version, for example 0.1.0" >&2
  exit 2
fi
[[ $target == macos ]] || { echo "this packaging script supports target 'macos' only" >&2; exit 2; }
[[ $architecture == aarch64 ]] || { echo "this packaging script supports architecture 'aarch64' only" >&2; exit 2; }
[[ -d $binary_dir ]] || { echo "binary directory does not exist: $binary_dir" >&2; exit 2; }
if [[ -n $settings_app_dir && ! -d $settings_app_dir ]]; then
  echo "settings application tree is not a directory: $settings_app_dir" >&2
  exit 2
fi
if [[ -n $settings_app_dir ]]; then
  settings_app_dir=$(cd "$settings_app_dir" && pwd)
fi

for name in gflick-setup gflick-agent gflick-tray gflick gflick-probe gflick-bench; do
  [[ -f $binary_dir/$name ]] || { echo "required release binary is missing: $binary_dir/$name" >&2; exit 2; }
done

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd "$script_dir/.." && pwd)
[[ -f $repository_root/assets/favicon.icns ]] || { echo "required tray icon is missing" >&2; exit 2; }
mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd)
temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/gflick-package.XXXXXX")
trap 'rm -rf "$temporary_root"' EXIT
user_stage=$temporary_root/user
developer_stage=$temporary_root/devtools
mkdir -p "$user_stage/payload/tray/Contents/MacOS" "$user_stage/payload/tray/Contents/Resources" "$developer_stage"

cp "$binary_dir/gflick-setup" "$user_stage/gflick-setup"
cp "$binary_dir/gflick-agent" "$user_stage/payload/gflick-agent"
cp "$binary_dir/gflick-tray" "$user_stage/payload/tray/Contents/MacOS/gflick-tray"
cp "$binary_dir/gflick" "$user_stage/payload/gflick"
cp "$repository_root/assets/favicon.icns" "$user_stage/payload/tray/Contents/Resources/favicon.icns"
cat > "$user_stage/payload/tray/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key>
  <string>GFlick</string>
  <key>CFBundleExecutable</key>
  <string>gflick-tray</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.r4ultv.gflick</string>
  <key>CFBundleIconFile</key>
  <string>favicon.icns</string>
  <key>CFBundleName</key>
  <string>GFlick</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
EOF
plutil -lint "$user_stage/payload/tray/Contents/Info.plist" >/dev/null

json_escape() {
  sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' <<<"$1"
}

file_record() {
  local absolute=$1 source=$2 root=$3 destination=$4 executable=$5
  local length digest
  length=$(stat -f '%z' "$absolute")
  digest=$(shasum -a 256 "$absolute" | awk '{print tolower($1)}')
  printf '{"source":"%s","root":"%s","destination":"%s","length":%s,"sha256":"%s","executable":%s}' \
    "$(json_escape "$source")" "$root" "$(json_escape "$destination")" "$length" "$digest" "$executable"
}

settings_fragment=$temporary_root/settings-files.json
settings_present=false
if [[ -n $settings_app_dir ]]; then
  settings_present=true
  mkdir -p "$user_stage/payload/settings"
  : > "$temporary_root/settings-unsorted"
  while IFS= read -r -d '' file; do
    if printf '%s' "$file" | LC_ALL=C grep -q '[[:cntrl:]]'; then
      echo "settings application paths must not contain control characters" >&2
      exit 2
    fi
    printf '%s\n' "$file" >> "$temporary_root/settings-unsorted"
  done < <(find "$settings_app_dir" -type f -print0)
  LC_ALL=C sort "$temporary_root/settings-unsorted" > "$temporary_root/settings-list"
  [[ -s $temporary_root/settings-list ]] || { echo "settings application tree contains no regular files" >&2; exit 2; }
  first=true
  printf '[' > "$settings_fragment"
  while IFS= read -r file; do
    relative=${file#"$settings_app_dir"/}
    [[ -n $relative && $relative != "$file" && $relative != *..* ]] || { echo "invalid settings application path: $relative" >&2; exit 2; }
    staged=$user_stage/payload/settings/$relative
    mkdir -p "$(dirname "$staged")"
    cp "$file" "$staged"
    executable=false
    [[ -x $file ]] && executable=true
    $first || printf ',' >> "$settings_fragment"
    file_record "$staged" "payload/settings/$relative" user_applications "$relative" "$executable" >> "$settings_fragment"
    first=false
  done < "$temporary_root/settings-list"
  printf ']' >> "$settings_fragment"
fi

setup_record=$(file_record "$user_stage/gflick-setup" gflick-setup private_bin gflick-setup true)
agent_record=$(file_record "$user_stage/payload/gflick-agent" payload/gflick-agent private_bin gflick-agent true)
tray_binary_record=$(file_record "$user_stage/payload/tray/Contents/MacOS/gflick-tray" payload/tray/Contents/MacOS/gflick-tray private_app Contents/MacOS/gflick-tray true)
tray_plist_record=$(file_record "$user_stage/payload/tray/Contents/Info.plist" payload/tray/Contents/Info.plist private_app Contents/Info.plist false)
tray_icon_record=$(file_record "$user_stage/payload/tray/Contents/Resources/favicon.icns" payload/tray/Contents/Resources/favicon.icns private_app Contents/Resources/favicon.icns false)
cli_record=$(file_record "$user_stage/payload/gflick" payload/gflick private_bin gflick true)

if $settings_present; then
  defaults='["agent","tray","settings"]'
  settings_component=",\"settings\":{\"files\":$(cat "$settings_fragment")}"
else
  defaults='["agent","tray"]'
  settings_component=''
fi
cat > "$user_stage/bundle.json" <<EOF
{
  "schema": 1,
  "product_version": "$version",
  "platform": "macos",
  "arch": "aarch64",
  "required": ["agent"],
  "defaults": $defaults,
  "setup": $setup_record,
  "components": {
    "agent": {"files": [$agent_record]},
    "tray": {"files": [$tray_binary_record,$tray_plist_record,$tray_icon_record]},
    "cli": {"files": [$cli_record]}$settings_component
  }
}
EOF

"$user_stage/gflick-setup" verify-bundle "$user_stage"

cp "$binary_dir/gflick-probe" "$developer_stage/gflick-probe"
cp "$binary_dir/gflick-bench" "$developer_stage/gflick-bench"
cat > "$developer_stage/README.md" <<'EOF'
# GFlick developer tools

These tools are not part of the user installation. `gflick-probe` opens HID
devices directly and must not run at the same time as `gflick-agent`.
EOF

find "$user_stage" "$developer_stage" -exec touch -t 198001010000 {} +
user_archive_name=gflick-$version-macos-aarch64.tar.gz
developer_archive_name=gflick-devtools-$version-macos-aarch64.tar.gz
user_archive=$output_dir/$user_archive_name
developer_archive=$output_dir/$developer_archive_name
(cd "$user_stage" && COPYFILE_DISABLE=1 tar -czf "$user_archive" bundle.json gflick-setup payload)
(cd "$developer_stage" && COPYFILE_DISABLE=1 tar -czf "$developer_archive" README.md gflick-probe gflick-bench)

fragment=$output_dir/SHA256SUMS-macos-aarch64
{
  shasum -a 256 "$user_archive" | awk -v name="$user_archive_name" '{print tolower($1) "  " name}'
  shasum -a 256 "$developer_archive" | awk -v name="$developer_archive_name" '{print tolower($1) "  " name}'
} > "$fragment"
