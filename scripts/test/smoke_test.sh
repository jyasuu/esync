#!/usr/bin/env bash
# scripts/test/smoke_test.sh
#
# Quick sanity check: starts the server, fires a few HTTP requests,
# checks the responses. No Rust test runner needed.
# Assumes infra is already running and data is seeded.
#
# Usage:
#   ./scripts/test/smoke_test.sh
#   GQL_PORT=4001 ./scripts/test/smoke_test.sh

set -euo pipefail

ES_URL="${ES_URL:-http://localhost:9200}"
GQL_URL="${GQL_URL:-http://localhost:4001/graphql}"
BINARY="${BINARY:-./target/debug/esync}"
CFG="${CFG:-esync.test.yaml}"

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'
BOLD='\033[1m'; NC='\033[0m'

pass() { echo -e "${GREEN}[PASS]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*"; FAILURES=$((FAILURES+1)); }
info() { echo -e "${CYAN}[INFO]${NC} $*"; }

FAILURES=0

# ── Build ─────────────────────────────────────────────────────────────────
echo -e "\n${BOLD}Building…${NC}"
cargo build --quiet

# ── Index ─────────────────────────────────────────────────────────────────
echo -e "\n${BOLD}── esync index ──${NC}"
ESYNC_CONFIG=$CFG $BINARY index 2>&1 | tail -5
pass "esync index completed"

# ── Serve in background ───────────────────────────────────────────────────
echo -e "\n${BOLD}── esync serve ──${NC}"
ESYNC_CONFIG=$CFG $BINARY serve --port 4001 &
SERVER_PID=$!
trap "kill $SERVER_PID 2>/dev/null || true" EXIT

info "Waiting for server (PID $SERVER_PID)…"
for i in $(seq 1 20); do
  curl -sf "$GQL_URL" -d '{"query":"{__typename}"}' \
       -H 'Content-Type: application/json' &>/dev/null && break
  sleep 0.5
done

# ── GraphQL: list ─────────────────────────────────────────────────────────
echo -e "\n${BOLD}── GraphQL: list_product ──${NC}"
RESP=$(curl -sf "$GQL_URL" \
  -H 'Content-Type: application/json' \
  -d '{"query":"{ list_product(limit:20) { id name price } }"}')

COUNT=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d['data']['list_product']))" 2>/dev/null || echo "0")
if [[ "$COUNT" -ge 1 ]]; then
  pass "list_product returned $COUNT products"
else
  fail "list_product returned 0 results — response: $RESP"
fi

# ── GraphQL: get by ID ────────────────────────────────────────────────────
echo -e "\n${BOLD}── GraphQL: get_product by id ──${NC}"
RESP=$(curl -sf "$GQL_URL" \
  -H 'Content-Type: application/json' \
  -d '{"query":"{ get_product(id: \"00000000-0000-0000-0000-000000000001\") { id name } }"}')

NAME=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['get_product']['name'])" 2>/dev/null || echo "")
if [[ "$NAME" == "Alpha Widget" ]]; then
  pass "get_product returned correct name: $NAME"
else
  fail "get_product name mismatch: got '$NAME' expected 'Alpha Widget'"
fi

# ── GraphQL: search ───────────────────────────────────────────────────────
echo -e "\n${BOLD}── GraphQL: search ──${NC}"
RESP=$(curl -sf "$GQL_URL" \
  -H 'Content-Type: application/json' \
  -d '{"query":"{ list_product(search: \"Widget\") { id name } }"}')

SEARCH_COUNT=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d['data']['list_product']))" 2>/dev/null || echo "0")
if [[ "$SEARCH_COUNT" -ge 1 ]]; then
  pass "search 'Widget' returned $SEARCH_COUNT results"
else
  fail "search 'Widget' returned 0 results — response: $RESP"
fi

# ── ES: direct index check ────────────────────────────────────────────────
echo -e "\n${BOLD}── Elasticsearch: index count ──${NC}"
ES_COUNT=$(curl -sf "$ES_URL/test_products/_count" | python3 -c "import sys,json; print(json.load(sys.stdin)['count'])" 2>/dev/null || echo "-1")
if [[ "$ES_COUNT" -ge 1 ]]; then
  pass "test_products index has $ES_COUNT documents"
else
  fail "test_products index has $ES_COUNT documents"
fi

# ── ES: search DSL ────────────────────────────────────────────────────────
echo -e "\n${BOLD}── Elasticsearch: DSL search ──${NC}"
ESYNC_CONFIG=$CFG $BINARY es search -i test_products -f examples/search-products.json \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(f\"hits: {d['hits']['total']['value']}\")"
pass "ES search DSL executed"

# ── ES: es index list ─────────────────────────────────────────────────────
echo -e "\n${BOLD}── esync es index list ──${NC}"
ESYNC_CONFIG=$CFG $BINARY es index list "test_*" | head -20
pass "es index list executed"

# ── Summary ───────────────────────────────────────────────────────────────
echo ""
if [[ $FAILURES -eq 0 ]]; then
  echo -e "${GREEN}${BOLD}All smoke tests passed ✓${NC}"
else
  echo -e "${RED}${BOLD}$FAILURES smoke test(s) FAILED${NC}"
  exit 1
fi
