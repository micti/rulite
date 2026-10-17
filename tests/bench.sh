#!/bin/sh
# T6.4 / T12.6 budget check: idle RSS, peak RSS under load, p50/p99 latency of
# the nested novel -> part query. Needs `oha` and `sqlite3`; run after
# `cargo build --release`.
#
# Usage: tests/bench.sh [seconds per run, default 10] [readers, default 1] [auth|tls]
# The server always runs with a config (anon and authenticated read every table) and
# every request carries the anon key; with `auth` it carries a signed-in user's access
# token instead. Either way, one HS256 verify per request. `tls` runs the build with
# `--features tls` (cargo build --release --features tls --target-dir target/tls) over
# HTTPS with the test certificate in tests/tls/.
set -eu

cd "$(dirname "$0")/.."
secs=${1:-10}
readers=${2:-1}
mode=${3:-}
bin=./target/release/rulite
tls_bin=./target/tls/release/rulite
scheme=http
set --
if [ "$mode" = tls ]; then
  bin=$tls_bin
  scheme=https
  set -- --tls-cert tests/tls/cert.pem --tls-key tests/tls/key.pem
fi
dir=$(mktemp -d)
trap 'kill $pid 2>/dev/null; rm -rf "$dir"' EXIT

sqlite3 "$dir/novel.db" < sample/novel.sql
printf '{"jwt_secret":"bench-secret-0123456789abcdef0123456789","tables":{"*":{"anon":"read","authenticated":"read"}}}' > "$dir/auth.json"
if [ "$mode" = auth ]; then
  printf 'bench@example.com\nsecret1\nsecret1\n' | $bin user add "$dir/novel.db" > /dev/null
fi
$bin "$dir/novel.db" --bind 127.0.0.1:0 --readers "$readers" --config "$dir/auth.json" "$@" 2>"$dir/err" &
pid=$!
until grep -q serving "$dir/err"; do sleep 0.1; done
url=$(sed -n "s/.* on \\($scheme:[^ ]*\\).*/\\1/p" "$dir/err")
query="$url/rest/v1/novel?select=title,edition(language,volume(number,chapter(title,part(content))))&edition.order=language"
anon=$($bin keys --config "$dir/auth.json" | sed -n 's/^anon *//p')
set -- -H "apikey: $anon"
if [ "$mode" = auth ]; then
  token=$(curl -sf -X POST -H "apikey: $anon" -H 'content-type: application/json' \
    -d '{"email":"bench@example.com","password":"secret1"}' "$url/auth/v1/token?grant_type=password" \
    | sed 's/.*"access_token":"\([^"]*\)".*/\1/')
  set -- -H "apikey: $anon" -H "Authorization: Bearer $token"
fi

rss() { ps -o rss= -p "$pid" | tr -d ' '; }
sleep 1
size() { [ -f "$1" ] && wc -c < "$1" | tr -d ' ' || echo "not built"; }
echo "binary:            $(size target/release/rulite) bytes"
echo "binary (tls):      $(size $tls_bin) bytes"
echo "idle RSS:          $(rss) KB"
curl -sf --cacert tests/tls/cert.pem "$@" "$query" > /dev/null
echo "RSS after 1 req:   $(rss) KB"

# Sample RSS every 100 ms while oha runs.
(peak=0; while kill -0 "$pid" 2>/dev/null; do r=$(rss); [ "${r:-0}" -gt "$peak" ] && peak=$r && echo "$peak" > "$dir/peak"; sleep 0.1; done) &
for c in 1 10 50; do
  echo "--- $c connection(s), ${secs}s"
  oha -z "${secs}s" -c "$c" --no-tui --insecure "$@" "$query" \
    | grep -E 'Success rate|Requests/sec|50.00% in|99.00% in|99.90% in|Slowest' | sed 's/^ */  /'
done
echo "peak RSS:          $(cat "$dir/peak") KB"
echo "RSS after load:    $(rss) KB"
echo "CPU time:          $(ps -o time= -p "$pid" | tr -d ' ')"
