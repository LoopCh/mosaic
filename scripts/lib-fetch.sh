# Shared helpers for get-engines-*.sh (sourced).
# Finds a release asset by regex through the GitHub API and downloads it via the API
# asset URL (works with the token, avoids broken CDN redirects), with retries.
auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $GITHUB_TOKEN"); fi
CURL=(curl -fSL --retry 6 --retry-delay 5 --retry-all-errors --connect-timeout 20)

# fetch_asset <owner/repo> <regex for the asset name> <output file>
# Takes the newest stable release that has the asset: a release published minutes ago
# may still be uploading its files (its asset list is empty), then the previous one is used.
fetch_asset() {
    local repo="$1" re="$2" out="$3" json pick url
    json="$("${CURL[@]}" -s "${auth[@]}" "https://api.github.com/repos/$repo/releases?per_page=10")"
    pick="$(jq -c --arg re "$re" '
        [ .[] | select(.draft | not) | select(.prerelease | not)
          | {tag: .tag_name, a: ([.assets[] | select(.name | test($re))][0])}
          | select(.a != null) ][0] // empty' <<<"$json")"
    if [ -z "$pick" ]; then
        echo "No asset matching /$re/ in the last releases of $repo. Latest assets:" >&2
        jq -r '[.[] | select(.prerelease | not)][0] | .tag_name, (.assets[].name)' <<<"$json" >&2
        return 1
    fi
    url="$(jq -r .a.url <<<"$pick")"
    echo "$repo $(jq -r .tag <<<"$pick"): $(jq -r .a.name <<<"$pick")"
    "${CURL[@]}" "${auth[@]}" -H "Accept: application/octet-stream" -o "$out" "$url"
}
