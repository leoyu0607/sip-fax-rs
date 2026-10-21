#!/usr/bin/env bash
# Run the crates/store behaviour suite against real database servers in Docker (M3a).
#   scripts/db-matrix.sh                    # postgres mysql mariadb mssql
#   scripts/db-matrix.sh postgres mariadb   # a subset
#   KEEP=1 scripts/db-matrix.sh             # leave the containers running
#   TZ_SERVER=Asia/Taipei scripts/db-matrix.sh   # timezone inside the DB containers (default Asia/Taipei)
#   NAME_PREFIX=mine PORT_OFFSET=300 scripts/db-matrix.sh   # own container names and host ports, so that two
#                                                           # checkouts on one Docker host do not remove each other's containers
# The same suite runs on SQLite in the normal `cargo test`; CI does this in the `db-matrix` job.
set -uo pipefail
cd "$(dirname "$0")/.."
command -v docker >/dev/null && docker info >/dev/null 2>&1 || { echo "docker is required" >&2; exit 2; }
BACKENDS=("$@"); [ ${#BACKENDS[@]} -eq 0 ] && BACKENDS=(postgres mysql mariadb mssql)
TZ_SERVER="${TZ_SERVER:-Asia/Taipei}"
NAME_PREFIX="${NAME_PREFIX:-sipfax-test}"
PORT_OFFSET="${PORT_OFFSET:-0}"
PW='Sipfax_Test_1'
declare -a STARTED=(); FAIL=0

start() { # name image port-map env... ; sets nothing, starts container
  local name=$1; shift; docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" "$@" >/dev/null && STARTED+=("$name")
}
wait_for() { # name, command...
  local name=$1; shift
  for _ in $(seq 1 90); do docker exec "$name" "$@" >/dev/null 2>&1 && return 0; sleep 2; done
  echo "timeout waiting for $name" >&2; docker logs --tail 20 "$name" >&2; return 1
}

for be in "${BACKENDS[@]}"; do
  name="$NAME_PREFIX-$be"
  case "$be" in
    postgres)
      start "$name" -e POSTGRES_PASSWORD=$PW -e TZ="$TZ_SERVER" -e PGTZ="$TZ_SERVER" -p $((15432 + PORT_OFFSET)):5432 postgres:16
      wait_for "$name" pg_isready -U postgres && sleep 3
      url="postgres://postgres:$PW@127.0.0.1:$((15432 + PORT_OFFSET))/postgres" ;;
    mysql)
      start "$name" -e MYSQL_ROOT_PASSWORD=$PW -e TZ="$TZ_SERVER" -p $((13306 + PORT_OFFSET)):3306 mysql:8.0
      wait_for "$name" mysql -uroot -p$PW -e 'SELECT 1' && sleep 3
      url="mysql://root:$PW@127.0.0.1:$((13306 + PORT_OFFSET))/mysql" ;;
    mariadb)
      start "$name" -e MARIADB_ROOT_PASSWORD=$PW -e TZ="$TZ_SERVER" -p $((13307 + PORT_OFFSET)):3306 mariadb:11
      wait_for "$name" mariadb -uroot -p$PW -e 'SELECT 1' && sleep 3
      url="mariadb://root:$PW@127.0.0.1:$((13307 + PORT_OFFSET))/mysql" ;;
    mssql)
      start "$name" --platform linux/amd64 -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD=$PW -e TZ="$TZ_SERVER" -p $((11433 + PORT_OFFSET)):1433 mcr.microsoft.com/mssql/server:2022-latest
      wait_for "$name" /opt/mssql-tools18/bin/sqlcmd -C -S localhost -U sa -P $PW -Q 'SELECT 1' && sleep 3
      url="mssql://sa:$PW@127.0.0.1:$((11433 + PORT_OFFSET))/master?trust_server_certificate=true" ;;
    *) echo "unknown backend $be" >&2; exit 2 ;;
  esac
  echo "=== store suite on $be ($url) ==="
  if SIP_FAXD_TEST_DB="$url" SIP_FAXD_TEST_DB_CONTAINER="$name" cargo test -p store -- --test-threads="${TEST_THREADS:-4}" 2>&1 | tee "target/db-matrix-$be.log" | tail -n 12; then
    grep -q "test result: FAILED" "target/db-matrix-$be.log" && { echo "FAILED: $be"; FAIL=1; } || echo "PASSED: $be"
  else
    echo "FAILED: $be"; FAIL=1
  fi
  # M4a R04: the database restarts while faxes are received and delivered
  echo "=== R04 (database restart during delivery) on $be ==="
  if SIP_FAXD_TEST_DB="$url" SIP_FAXD_TEST_DB_CONTAINER="$name" cargo test -p sip-faxd --bin sip-faxd r04 -- --nocapture 2>&1 | tee "target/db-matrix-$be-r04.log" | tail -n 6; then
    grep -q "test result: ok. 1 passed" "target/db-matrix-$be-r04.log" && echo "PASSED: $be r04" || { echo "FAILED: $be r04"; FAIL=1; }
  else
    echo "FAILED: $be r04"; FAIL=1
  fi
done
if [ -z "${KEEP:-}" ]; then for n in "${STARTED[@]}"; do docker rm -f "$n" >/dev/null 2>&1; done; fi
exit $FAIL
