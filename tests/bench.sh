#!/bin/sh
# T6.4 / T12.6 budget check: idle RSS, peak RSS under load, p50/p99 latency of
# the nested novel -> part query. Needs `oha` and `sqlite3`; run after
# `cargo build --release`.
#
# Usage: tests/bench.sh [seconds per run, default 10] [readers, default 1] [auth]
# With `auth`, the server has a JWT secret and every request carries a signed-in
# user's access token (one HS256 verify per request).
set -eu

cd "$(dirname "$0")/.."
secs=${1:-10}
readers=${2:-1}
mode=${3:-}
bin=./target/release/rulite
dir=$(mktemp -d)
trap 'kill $pid 2>/dev/null; rm -rf "$dir"' EXIT

sqlite3 "$dir/novel.db" < sample/novel.sql
set --
if [ "$mode" = auth ]; then
  printf '{"jwt_secret":"bench-secret-0123456789abcdef0123456789","tables":{"*":{"anon":"read","authenticated":"read"}}}' > "$dir/auth.json"
  printf 'bench@example.com\nsecret1\nsecret1\n' | $bin user add "$dir/novel.db" > /dev/null
  set -- --config "$dir/auth.json"
fi
$bin "$dir/novel.db" --bind 127.0.0.1:0 --readers "$readers" "$@" 2>"$dir/err" &
pid=$!
until grep -q serving "$dir/err"; do sleep 0.1; done
url=$(sed -n 's/.* on \(http:[^ ]*\).*/\1/p' "$dir/err")
query="$url/rest/v1/novel?select=title,edition(language,volume(number,chapter(title,part(content))))&edition.order=language"
if [ "$mode" = auth ]; then
  anon=$($bin keys --config "$dir/auth.json" | sed -n 's/^anon *//p')
  token=$(curl -sf -X POST -H "apikey: $anon" -H 'content-type: application/json' \
    -d '{"email":"bench@example.com","password":"secret1"}' "$url/auth/v1/token?grant_type=password" \
    | sed 's/.*"access_token":"\([^"]*\)".*/\1/')
  set -- -H "apikey: $anon" -H "Authorization: Bearer $token"
fi

rss() { ps -o rss= -p "$pid" | tr -d ' '; }
sleep 1
echo "binary:            $(wc -c < target/release/rulite | tr -d ' ') bytes"
echo "idle RSS:          $(rss) KB"
curl -sf "$@" "$query" > /dev/null
echo "RSS after 1 req:   $(rss) KB"

# Sample RSS every 100 ms while oha runs.
(peak=0; while kill -0 "$pid" 2>/dev/null; do r=$(rss); [ "${r:-0}" -gt "$peak" ] && peak=$r && echo "$peak" > "$dir/peak"; sleep 0.1; done) &
for c in 1 10 50; do
  echo "--- $c connection(s), ${secs}s"
  oha -z "${secs}s" -c "$c" --no-tui "$@" "$query" \
    | grep -E 'Success rate|Requests/sec|50.00% in|99.00% in|99.90% in|Slowest' | sed 's/^ */  /'
done
echo "peak RSS:          $(cat "$dir/peak") KB"
echo "RSS after load:    $(rss) KB"
echo "CPU time:          $(ps -o time= -p "$pid" | tr -d ' ')"
