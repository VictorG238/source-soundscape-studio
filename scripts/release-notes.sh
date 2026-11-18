#!/bin/sh
set -eu

tag=${1:?Expected a version tag}
package=$(cargo pkgid --locked -p source-soundscape-studio)
version=${package##*#}
version=${version##*@}
if ! printf '%s\n' "$tag" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || [ "$tag" != "v$version" ]; then
    printf 'Tag %s must match the stable Cargo version v%s\n' "$tag" "$version" >&2
    exit 1
fi

notes=$(awk -v heading="## $version" '
    $0 == heading { active = 1; next }
    /^## / && active { exit }
    active { print }
' CHANGELOG.md)
if [ -z "$notes" ]; then
    printf 'Missing changelog entry for %s\n' "$version" >&2
    exit 1
fi
printf '# Source Soundscape Studio %s\n\nReleased %s (UTC).\n\n' "$version" "$(date -u +%F)"
printf '%s\n\n' "$notes"
printf 'Windows x64 portable build. Requires a compatible OpenGL driver and an audio output device for previews.\n'
