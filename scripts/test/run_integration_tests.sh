#!/usr/bin/env bash
# scripts/test/run_integration_tests.sh
#
# Full integration test runner.
# Starts Docker infra, seeds the test DB, runs all Rust tests, then tears down.
#
# Usage:
#   ./scripts/test/run_integration_tests.sh           # full run
#   ./scripts/test/run_integration_tests.sh --no-down # keep infra running after
#   ./scripts/test/run_integration_tests.sh --filter test_mapping  # run specific tests

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────
PG_HOST="${PG_HOST:-localhost}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-esync}"
PG_PASS="${PG_PASS:-esync}"
PG_TEST_DB="${PG_TEST_DB:-esync_test}"

ES_HOST="${ES_HOST:-localhost}"
ES_PORT="${ES_PORT:-9200}"

TEARDOWN=true
FILTER=""

for arg in "$@"; do
  case $arg in
    --no-down)   TEARDOWN=false ;;
    --filter=*)  FILTER="${arg#--filter=}" ;;
    --filter)    shift; FILTER="$1" ;;
  esac
done

# ── Colors ────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

info()    { echo -e "${CYAN}[INFO]${NC}  $*"; }
success() { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*"; }
header()  { echo -e "\n${BOLD}${CYAN}══ $* ══${NC}"; }

# ── Cleanup on exit ───────────────────────────────────────────────────────
cleanup() {
  local exit_code=$?
  if [[ "$TEARDOWN" == "true" ]]; then
    header "Tearing down infrastructure"
    docker compose -f docker-compose.yml \
      -f docker-compose.test.yml \
      down --remove-orphans 2>/dev/null || true
    info "Infrastructure stopped."
  else
    warn "Leaving infra running (--no-down). Run 'docker compose down' when done."
  fi
  if [[ $exit_code -eq 0 ]]; then
    success "All tests passed ✓"
  else
    error "Tests FAILED (exit $exit_code)"
  fi
}
trap cleanup EXIT

# ── Step 1: Start infra ───────────────────────────────────────────────────
header "Starting Docker infrastructure"
docker compose -f docker-compose.yml \
               -f docker-compose.test.yml \
               up -d postgres elasticsearch
info "Containers started."

# ── Step 2: Wait for Postgres ─────────────────────────────────────────────
header "Waiting for PostgreSQL"
MAX=30; COUNT=0
until PGPASSWORD="$PG_PASS" psql -h "$PG_HOST" -p "$PG_PORT" \
      -U "$PG_USER" -d postgres -c '\q' 2>/dev/null; do
  COUNT=$((COUNT+1))
  if [[ $COUNT -ge $MAX ]]; then error "Postgres did not become ready."; exit 1; fi
  echo -n "."
  sleep 1
done
echo ""
success "PostgreSQL is ready."

# ── Step 3: Create test database ──────────────────────────────────────────
header "Setting up test database: $PG_TEST_DB"
PGPASSWORD="$PG_PASS" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
  -c "DROP DATABASE IF EXISTS ${PG_TEST_DB};" \
  -c "CREATE DATABASE ${PG_TEST_DB};"

PGPASSWORD="$PG_PASS" psql -h "$PG_HOST" -p "$PG_PORT" \
  -U "$PG_USER" -d "$PG_TEST_DB" \
  -f scripts/test/setup_test_db.sql
success "Test database ready."

# ── Step 4: Wait for Elasticsearch ───────────────────────────────────────
header "Waiting for Elasticsearch"
MAX=60; COUNT=0
until curl -sf "http://${ES_HOST}:${ES_PORT}/_cluster/health" \
      | grep -qv '"status":"red"'; do
  COUNT=$((COUNT+1))
  if [[ $COUNT -ge $MAX ]]; then error "Elasticsearch did not become ready."; exit 1; fi
  echo -n "."
  sleep 2
done
echo ""
success "Elasticsearch is ready."

# ── Step 5: Clean up test indices from previous runs ─────────────────────
header "Cleaning stale test indices"
for idx in test_products test_orders it_create_delete it_recreate \
           it_get_index it_doc_crud it_doc_delete it_bulk it_search \
           it_mappings; do
  curl -sf -X DELETE "http://${ES_HOST}:${ES_PORT}/${idx}" 2>/dev/null || true
done
success "Stale indices removed."

# ── Step 6: Run tests ─────────────────────────────────────────────────────
header "Running integration tests"

ESYNC_CONFIG=esync.test.yaml
export ESYNC_CONFIG

if [[ -n "$FILTER" ]]; then
  info "Filter: $FILTER"
  cargo test --test '*' -- "$FILTER" --nocapture 2>&1
else
  cargo test --test '*' -- --nocapture 2>&1
fi

success "Test run complete."
