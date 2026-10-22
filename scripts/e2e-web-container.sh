#!/usr/bin/env bash
# Runs INSIDE the test container (started by scripts/e2e-web.sh): two sip-faxd instances with the
# web interface, their accounts (made with the CLI) and the source TIFFs for the SpanDSP sender.
#   instance a: web http  on 0.0.0.0:8080, SIP 127.0.0.1:5060 (UDP), database /tmp/e2e/a/fax.db
#   instance b: web https on 0.0.0.0:8443 (self-signed), SIP 127.0.0.1:5061, /tmp/e2e/b/fax.db
set -euo pipefail
BIN=/work/target/docker/debug
FAXD=$BIN/sip-faxd
PW='Correct-horse-battery-1'
for p in /tmp/e2e/*/pid; do [ -f "$p" ] && kill "$(cat "$p")" 2>/dev/null || true; done
sleep 1
rm -rf /tmp/e2e && mkdir -p /tmp/e2e

instance() {   # name sip-port web-bind scheme media-range
  local d=/tmp/e2e/$1
  mkdir -p "$d/run" "$d/data"
  cat > "$d/sip-faxd.toml" <<EOF
[log]
level = "info"
file = ""

[service]
pid_file = "$d/run/sip-faxd.pid"
socket = "$d/run/sip-faxd.sock"

[sip]
bind = "127.0.0.1:$2"
transports = ["udp"]

[media]
port_range = "$5"

[call]
station_id = "0287654321"

[web]
enabled = true
bind = "$3"
scheme = "$4"

[storage]
data_dir = "$d/data"

[capture]
enabled = false

[database]
url = "sqlite://$d/fax.db"

[[trunks]]
name = "gw"
allowed_sources = ["127.0.0.0/8"]
EOF
  local cfg=$d/sip-faxd.toml
  # accounts first: with a user in the table the server does not make its own `admin`
  add() { printf '%s\n' "$PW" | $FAXD user add "$@" --password-stdin --config "$cfg" >/dev/null; }
  add root --role admin
  add amy --role agent
  add ben --role agent --display-name Ben
  add newbie --role agent --must-change
  nohup $FAXD serve --config "$cfg" > "$d/serve.log" 2>&1 &
  echo $! > "$d/pid"
  for _ in $(seq 1 120); do
    grep -q "sip-faxd ready" "$d/serve.log" 2>/dev/null && return 0
    sleep 0.5
  done
  echo "instance $1 did not become ready:" >&2; cat "$d/serve.log" >&2; exit 1
}

instance a 5060 0.0.0.0:8080 http 31000-31199
instance b 5061 0.0.0.0:8443 https 31200-31399

# 1, 3 pages: tall fine pages so that each page takes SpanDSP a few seconds (the page counter
# of the list can be watched); a short one-page fax for the tests that only need a record
$BIN/make_tiff /tmp/e2e/one.tif 1 300 fine
$BIN/make_tiff /tmp/e2e/three.tif 3 1500 fine
echo "e2e-web: instances ready"
