# Shared helpers for get-engines-*.sh (sourced).
# Finds a release asset by regex through the GitHub API and downloads it via the API
# asset URL (works with the token, avoids broken CDN redirects), with retries.
auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $GITHUB_TOKEN"); fi
CURL=(curl -fSL --retry 6 --retry-delay 5 --retry-all-errors --connect-timeout 20)

# fetch_asset <owner/repo> <regex for the asset name> <output file>
fetch_asset() {
    local repo="$1" re="$2" out="$3" json url tag
    json="$("${CURL[@]}" -s "${auth[@]}" "https://api.github.com/repos/$repo/releases/latest")"
    tag="$(jq -r .tag_name <<<"$json")"
    url="$(jq -r --arg re "$re" '[.assets[] | select(.name | test($re))][0].url // empty' <<<"$json")"
    if [ -z "$url" ]; then
        echo "No asset matching /$re/ in $repo $tag. Assets:" >&2
        jq -r '.assets[].name' <<<"$json" >&2
        return 1
    fi
    echo "$repo $tag: $(jq -r --arg re "$re" '[.assets[] | select(.name | test($re))][0].name' <<<"$json")"
    "${CURL[@]}" "${auth[@]}" -H "Accept: application/octet-stream" -o "$out" "$url"
}
