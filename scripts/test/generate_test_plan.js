const {
  Document, Packer, Paragraph, TextRun, Table, TableRow, TableCell,
  HeadingLevel, AlignmentType, BorderStyle, WidthType, ShadingType,
  LevelFormat, Footer, SimpleField
} = require('docx');
const fs = require('fs');

// ── Helpers ───────────────────────────────────────────────────────────────

const CONTENT_WIDTH = 9360; // 8.5" - 2×1" margins in DXA

const border = { style: BorderStyle.SINGLE, size: 1, color: "CCCCCC" };
const borders = { top: border, bottom: border, left: border, right: border };

const headerBg   = { fill: "1E3A5F", type: ShadingType.CLEAR };  // dark navy
const stripeBg   = { fill: "F2F6FB", type: ShadingType.CLEAR };  // light blue tint
const passBg     = { fill: "D4EDDA", type: ShadingType.CLEAR };  // soft green
const failBg     = { fill: "F8D7DA", type: ShadingType.CLEAR };  // soft red
const warnBg     = { fill: "FFF3CD", type: ShadingType.CLEAR };  // amber

function cell(text, opts = {}) {
  const { bold = false, color = "333333", bg = null, align = AlignmentType.LEFT, width = null } = opts;
  return new TableCell({
    borders,
    ...(bg ? { shading: bg } : {}),
    ...(width ? { width: { size: width, type: WidthType.DXA } } : {}),
    margins: { top: 80, bottom: 80, left: 120, right: 120 },
    children: [new Paragraph({
      alignment: align,
      children: [new TextRun({ text, bold, color, size: 18 })]
    })]
  });
}

function headerCell(text, width) {
  return new TableCell({
    borders,
    shading: headerBg,
    width: { size: width, type: WidthType.DXA },
    margins: { top: 100, bottom: 100, left: 140, right: 140 },
    children: [new Paragraph({
      children: [new TextRun({ text, bold: true, color: "FFFFFF", size: 18 })]
    })]
  });
}

function h1(text) {
  return new Paragraph({
    heading: HeadingLevel.HEADING_1,
    spacing: { before: 320, after: 160 },
    children: [new TextRun({ text, bold: true, color: "1E3A5F", size: 32 })]
  });
}

function h2(text) {
  return new Paragraph({
    heading: HeadingLevel.HEADING_2,
    spacing: { before: 240, after: 120 },
    children: [new TextRun({ text, bold: true, color: "2E5F8E", size: 26 })]
  });
}

function h3(text) {
  return new Paragraph({
    heading: HeadingLevel.HEADING_3,
    spacing: { before: 200, after: 80 },
    children: [new TextRun({ text, bold: true, color: "3A7CBD", size: 22 })]
  });
}

function para(runs) {
  const children = typeof runs === 'string'
    ? [new TextRun({ text: runs, size: 20, color: "333333" })]
    : runs;
  return new Paragraph({ spacing: { after: 120 }, children });
}

function mono(text) {
  return new TextRun({ text, font: "Courier New", size: 18, color: "C0392B" });
}

function note(text, bg = warnBg) {
  return new Table({
    width: { size: CONTENT_WIDTH, type: WidthType.DXA },
    columnWidths: [CONTENT_WIDTH],
    margins: { top: 80, bottom: 160 },
    rows: [new TableRow({
      children: [new TableCell({
        borders, shading: bg,
        margins: { top: 80, bottom: 80, left: 180, right: 180 },
        children: [new Paragraph({ children: [new TextRun({ text, size: 18, color: "555555", italics: true })] })]
      })]
    })]
  });
}

function spacer(size = 120) {
  return new Paragraph({ spacing: { after: size }, children: [] });
}

function bullet(text, ref = "bullets") {
  return new Paragraph({
    numbering: { reference: ref, level: 0 },
    spacing: { after: 60 },
    children: [new TextRun({ text, size: 20, color: "333333" })]
  });
}

function testTable(rows) {
  const colWidths = [620, 2200, 3200, 1900, 1440];  // ID, Name, Description, Command, Expected
  return new Table({
    width: { size: CONTENT_WIDTH, type: WidthType.DXA },
    columnWidths: colWidths,
    rows: [
      new TableRow({
        tableHeader: true,
        children: [
          headerCell("ID",          colWidths[0]),
          headerCell("Test Name",   colWidths[1]),
          headerCell("Description", colWidths[2]),
          headerCell("Command / Query", colWidths[3]),
          headerCell("Expected",    colWidths[4]),
        ]
      }),
      ...rows.map((r, i) => new TableRow({
        children: [
          cell(r[0], { bg: i % 2 ? stripeBg : null, align: AlignmentType.CENTER }),
          cell(r[1], { bold: true, bg: i % 2 ? stripeBg : null }),
          cell(r[2], { bg: i % 2 ? stripeBg : null }),
          cell(r[3], { bg: i % 2 ? stripeBg : null }),
          cell(r[4], { bg: i % 2 ? stripeBg : null }),
        ]
      }))
    ]
  });
}

function statusTable(rows) {
  const colWidths = [620, 2800, 1680, 1680, 2580];
  return new Table({
    width: { size: CONTENT_WIDTH, type: WidthType.DXA },
    columnWidths: colWidths,
    rows: [
      new TableRow({
        tableHeader: true,
        children: [
          headerCell("ID",       colWidths[0]),
          headerCell("Test Name", colWidths[1]),
          headerCell("Suite",    colWidths[2]),
          headerCell("Priority", colWidths[3]),
          headerCell("Notes",    colWidths[4]),
        ]
      }),
      ...rows.map((r, i) => new TableRow({
        children: [
          cell(r[0], { bg: i % 2 ? stripeBg : null, align: AlignmentType.CENTER }),
          cell(r[1], { bold: true, bg: i % 2 ? stripeBg : null }),
          cell(r[2], { bg: i % 2 ? stripeBg : null }),
          cell(r[3], { bg: i % 2 ? stripeBg : null, align: AlignmentType.CENTER }),
          cell(r[4], { bg: i % 2 ? stripeBg : null }),
        ]
      }))
    ]
  });
}

// ── Document ──────────────────────────────────────────────────────────────

const doc = new Document({
  numbering: {
    config: [
      {
        reference: "bullets",
        levels: [{ level: 0, format: LevelFormat.BULLET, text: "•",
          alignment: AlignmentType.LEFT,
          style: { paragraph: { indent: { left: 720, hanging: 360 } } } }]
      },
      {
        reference: "numbers",
        levels: [{ level: 0, format: LevelFormat.DECIMAL, text: "%1.",
          alignment: AlignmentType.LEFT,
          style: { paragraph: { indent: { left: 720, hanging: 360 } } } }]
      }
    ]
  },
  styles: {
    default: {
      document: { run: { font: "Arial", size: 20, color: "333333" } }
    },
    paragraphStyles: [
      { id: "Heading1", name: "Heading 1", basedOn: "Normal", next: "Normal", quickFormat: true,
        run:  { size: 32, bold: true, font: "Arial", color: "1E3A5F" },
        paragraph: { spacing: { before: 320, after: 160 }, outlineLevel: 0 } },
      { id: "Heading2", name: "Heading 2", basedOn: "Normal", next: "Normal", quickFormat: true,
        run:  { size: 26, bold: true, font: "Arial", color: "2E5F8E" },
        paragraph: { spacing: { before: 240, after: 120 }, outlineLevel: 1 } },
      { id: "Heading3", name: "Heading 3", basedOn: "Normal", next: "Normal", quickFormat: true,
        run:  { size: 22, bold: true, font: "Arial", color: "3A7CBD" },
        paragraph: { spacing: { before: 200, after: 80 }, outlineLevel: 2 } },
    ]
  },
  sections: [{
    properties: {
      page: {
        size: { width: 12240, height: 15840 },
        margin: { top: 1440, right: 1260, bottom: 1440, left: 1260 }
      }
    },
    footers: {
      default: new Footer({
        children: [new Paragraph({
          alignment: AlignmentType.RIGHT,
          children: [
            new TextRun({ text: "esync Integration Test Plan  |  Page ", size: 16, color: "999999" }),
            new SimpleField("PAGE"),
          ]
        })]
      })
    },
    children: [

      // ── Cover ────────────────────────────────────────────────────────
      new Paragraph({
        spacing: { before: 1440, after: 240 },
        children: [new TextRun({ text: "esync", bold: true, size: 72, color: "1E3A5F" })]
      }),
      new Paragraph({
        spacing: { after: 120 },
        children: [new TextRun({ text: "Integration Test Plan", size: 40, color: "2E5F8E" })]
      }),
      new Paragraph({
        spacing: { after: 600 },
        children: [new TextRun({ text: "PostgreSQL  →  GraphQL  →  Elasticsearch", size: 24, color: "888888", italics: true })]
      }),
      new Table({
        width: { size: 5000, type: WidthType.DXA },
        columnWidths: [2000, 3000],
        rows: [
          new TableRow({ children: [cell("Version", { bold: true, bg: stripeBg }), cell("0.1.0")] }),
          new TableRow({ children: [cell("Date",    { bold: true, bg: stripeBg }), cell(new Date().toISOString().slice(0,10))] }),
          new TableRow({ children: [cell("Scope",   { bold: true, bg: stripeBg }), cell("Mapping · ES Client · Index · CDC · GraphQL")] }),
          new TableRow({ children: [cell("Runtime", { bold: true, bg: stripeBg }), cell("Postgres 16 · Elasticsearch 8.13 · Rust 1.78")] }),
        ]
      }),
      spacer(800),

      // ── 1. Introduction ───────────────────────────────────────────────
      h1("1. Introduction"),
      para("This document defines the integration test strategy and individual test cases for esync — a Rust CLI that bridges PostgreSQL and Elasticsearch via a dynamic GraphQL layer."),
      para("The test suite validates five distinct concerns:"),
      bullet("Type mapping derivation (PgType → ES field type)"),
      bullet("Elasticsearch client operations (index, document, bulk, template, ILM policy)"),
      bullet("Full index rebuild command (Postgres → ES)"),
      bullet("CDC watch loop (Postgres LISTEN/NOTIFY → ES upsert/delete)"),
      bullet("GraphQL server (list, get, search, pagination, soft-delete filter)"),
      spacer(),

      // ── 2. Test environment ───────────────────────────────────────────
      h1("2. Test Environment"),

      h2("2.1 Infrastructure"),
      new Table({
        width: { size: CONTENT_WIDTH, type: WidthType.DXA },
        columnWidths: [2400, 2400, 4560],
        rows: [
          new TableRow({ tableHeader: true, children: [
            headerCell("Component",  2400),
            headerCell("Version",    2400),
            headerCell("Connection", 4560),
          ]}),
          new TableRow({ children: [
            cell("PostgreSQL"),
            cell("16-alpine"),
            cell("postgres://esync:esync@localhost:5432/esync_test"),
          ]}),
          new TableRow({ children: [
            cell("Elasticsearch", { bg: stripeBg }),
            cell("8.13.0", { bg: stripeBg }),
            cell("http://localhost:9200 (security disabled)", { bg: stripeBg }),
          ]}),
          new TableRow({ children: [
            cell("Rust toolchain"),
            cell("stable ≥ 1.78"),
            cell("cargo test --test <suite>"),
          ]}),
        ]
      }),
      spacer(),

      h2("2.2 Configuration File"),
      para([
        new TextRun({ text: "All integration tests use ", size: 20, color: "333333" }),
        mono("esync.test.yaml"),
        new TextRun({ text: " (loaded via the ", size: 20, color: "333333" }),
        mono("ESYNC_CONFIG"),
        new TextRun({ text: " environment variable). Key differences from the dev config:", size: 20, color: "333333" }),
      ]),
      bullet("Database: esync_test (isolated from dev data)"),
      bullet("ES index names prefixed with test_ (test_products, test_orders)"),
      bullet("GraphQL server binds to port 4001"),
      bullet("batch_size reduced to 10 to exercise multi-batch pagination code paths"),
      spacer(),

      h2("2.3 Test Database Setup"),
      para([
        new TextRun({ text: "Run ", size: 20, color: "333333" }),
        mono("scripts/test/setup_test_db.sql"),
        new TextRun({ text: " once to create the schema, CDC triggers, and the ", size: 20, color: "333333" }),
        mono("seed_test_data()"),
        new TextRun({ text: " stored procedure. Each test suite calls ", size: 20, color: "333333" }),
        mono("reseed()"),
        new TextRun({ text: " at the start of destructive tests to restore a known state.", size: 20, color: "333333" }),
      ]),
      spacer(),

      h2("2.4 Running the Tests"),
      new Table({
        width: { size: CONTENT_WIDTH, type: WidthType.DXA },
        columnWidths: [3800, 5560],
        rows: [
          new TableRow({ tableHeader: true, children: [
            headerCell("Command", 3800),
            headerCell("What it runs", 5560),
          ]}),
          new TableRow({ children: [cell("scripts/test/run_integration_tests.sh"), cell("Full suite: starts infra, seeds DB, runs all tests, tears down")] }),
          new TableRow({ children: [cell("scripts/test/smoke_test.sh", { bg: stripeBg }), cell("Quick sanity check via HTTP — no Rust test runner required", { bg: stripeBg })] }),
          new TableRow({ children: [cell("cargo test --test test_mapping"), cell("Unit tests only — no infra required")] }),
          new TableRow({ children: [cell("cargo test --test test_elastic", { bg: stripeBg }), cell("ES client tests — ES only (no Postgres)", { bg: stripeBg })] }),
          new TableRow({ children: [cell("cargo test --test test_index_command"), cell("Index rebuild tests — Postgres + ES")] }),
          new TableRow({ children: [cell("cargo test --test test_watch_cdc", { bg: stripeBg }), cell("CDC watch tests — Postgres + ES", { bg: stripeBg })] }),
          new TableRow({ children: [cell("cargo test --test test_graphql"), cell("GraphQL server tests — Postgres only")] }),
        ]
      }),
      spacer(),

      // ── 3. Test cases: mapping ─────────────────────────────────────────
      h1("3. Test Suite: Mapping (test_mapping.rs)"),
      para("Pure unit tests with no external infrastructure. Validates that src/indexer/mapping.rs correctly derives Elasticsearch field types from Postgres column configs and assembles well-formed index body JSON."),
      note("These tests run in CI on every push without any Docker infrastructure."),
      spacer(80),
      testTable([
        ["M-01", "UUID → keyword",          "UUID columns must map to keyword for exact-match queries", "derive_es_type(UUID)", "EsFieldType::Keyword"],
        ["M-02", "TEXT → text",             "TEXT and VARCHAR map to text for full-text search",         "derive_es_type(TEXT)", "EsFieldType::Text"],
        ["M-03", "INT4 → integer",           "32-bit integers map to integer",                           "derive_es_type(INT4)", "EsFieldType::Integer"],
        ["M-04", "INT8 → long",              "64-bit integers map to long",                              "derive_es_type(INT8)", "EsFieldType::Long"],
        ["M-05", "NUMERIC → scaled_float",   "Arbitrary-precision decimals map to scaled_float (×100)", "derive_es_type(NUMERIC)", "EsFieldType::ScaledFloat"],
        ["M-06", "BOOL → boolean",           "Booleans map directly",                                   "derive_es_type(BOOL)", "EsFieldType::Boolean"],
        ["M-07", "TIMESTAMPTZ → date",       "All timestamp variants map to date",                      "derive_es_type(TIMESTAMPTZ)", "EsFieldType::Date"],
        ["M-08", "JSONB → object",           "JSON/JSONB maps to object",                               "derive_es_type(JSONB)", "EsFieldType::Object"],
        ["M-09", "es_type override",         "Explicit es_type in config takes precedence over pg_type","override TEXT→keyword", "EsFieldType::Keyword"],
        ["M-10", "keyword sub-field",        "text fields get a .keyword sub-field when enabled",       "build_mappings([TEXT])", "fields.keyword.type = keyword"],
        ["M-11", "no keyword sub-field",     "keyword_subfield=false omits sub-field",                  "keyword_subfield=false", "No fields.keyword key"],
        ["M-12", "UUID no sub-field",        "keyword fields should not get a .keyword sub-field",      "build_mappings([UUID])", "No fields key"],
        ["M-13", "date format string",       "date fields include the ES format string",                "build_mappings([TIMESTAMPTZ])", "strict_date_optional_time present"],
        ["M-14", "scaled_float factor",      "scaled_float includes scaling_factor: 100",               "build_mappings([NUMERIC])", "scaling_factor = 100"],
        ["M-15", "indexed=false excluded",   "Columns with indexed=false are omitted from mappings",    "indexed=false column", "Absent from properties"],
        ["M-16", "es_extra merged",          "Extra properties are merged verbatim into the field def", "es_extra: {copy_to}", "copy_to present in field"],
        ["M-17", "index body settings",      "build_index_body returns settings + mappings wrapper",    "build_index_body(1,0)", "settings.number_of_shards=1"],
      ]),
      spacer(),

      // ── 4. Test cases: ES client ──────────────────────────────────────
      h1("4. Test Suite: Elasticsearch Client (test_elastic.rs)"),
      para("Integration tests for src/elastic.rs using a live Elasticsearch instance. Each test creates and cleans up its own isolated index to allow parallel execution."),
      spacer(80),
      testTable([
        ["E-01", "Create and delete index",    "Full index lifecycle: create → verify exists → delete → verify gone",   "create_index / delete_index", "200 on create, false on exists after delete"],
        ["E-02", "Recreate drops existing",    "recreate_index drops the old index and creates fresh",                  "recreate_index",              "Documents from V1 absent after recreate"],
        ["E-03", "Get index metadata",         "get_index returns index metadata including mappings",                   "get_index",                   "Response contains index name key"],
        ["E-04", "Put and get document",       "Put a document, fetch it back, verify _source fields",                  "put_document / get_document", "_source fields match input"],
        ["E-05", "Delete document",            "Delete a document; subsequent get returns found:false",                 "delete_document",             "404 or found=false on re-fetch"],
        ["E-06", "Bulk index 25 docs",         "Bulk-index 25 documents in one request; verify count via _search",     "bulk_index",                  "25 hits after refresh"],
        ["E-07", "Bulk index empty slice",     "bulk_index with empty slice must not error or send a request",         "bulk_index(&[])",             "Ok(()) no error"],
        ["E-08", "Search returns matches",     "DSL term query returns only matching documents",                        "search with term filter",     "Count matches expected subset"],
        ["E-09", "Get and update mappings",    "put_mapping adds a new field; get_mapping shows it",                   "put_mapping / index_mappings","New field present in mappings"],
        ["E-10", "Index template lifecycle",   "Put, get, and delete an index template",                               "put/get/delete_template",     "Template present after put, gone after delete"],
        ["E-11", "ILM policy lifecycle",       "Put, get, and delete an ILM lifecycle policy",                         "put/get/delete_policy",       "Policy present after put, gone after delete"],
      ]),
      spacer(),

      // ── 5. Test cases: index command ──────────────────────────────────
      h1("5. Test Suite: Index Command (test_index_command.rs)"),
      para("End-to-end tests for the esync index command. Requires both Postgres (seeded) and Elasticsearch. Tests call indexer::rebuild_index() directly to avoid subprocess overhead."),
      spacer(80),
      testTable([
        ["I-01", "Rebuild indexes all rows",       "All 5 seeded products appear in ES after rebuild",                          "rebuild_index(Product)", "5 hits in test_products"],
        ["I-02", "Document fields match DB",       "ES _source fields match the values stored in Postgres exactly",             "get_document(PRODUCT_1)", "name, price, stock, active match"],
        ["I-03", "Rebuild replaces stale data",    "Update a row in DB, run rebuild, ES shows the new value",                  "UPDATE + rebuild",        "Modified name present in ES"],
        ["I-04", "SQL filter respected",           "Soft-deleted orders (deleted_at IS NOT NULL) are excluded from the index", "rebuild_index(Order)",   "2 hits, ORDER_1 absent"],
        ["I-05", "Multi-batch pagination",         "20 products with batch_size=10 exercises 2 full batch iterations",         "20 rows, batch=10",       "All 20 present in ES"],
        ["I-06", "Multiple entities independent",  "Rebuilding all entities produces correct counts per index",                 "rebuild all",             "5 products, 3 orders"],
      ]),
      spacer(),

      // ── 6. Test cases: CDC watch ──────────────────────────────────────
      h1("6. Test Suite: CDC Watch (test_watch_cdc.rs)"),
      para("Tests for the esync watch command which listens to Postgres NOTIFY channels and keeps Elasticsearch in sync in real time."),
      para("Each test spawns the watch loop as a background Tokio task. After performing a DB mutation it polls ES every 200ms until the expected state appears or a 5–8 second timeout elapses."),
      note("CDC tests are inherently time-sensitive. If they fail intermittently on slow CI machines, increase the timeout constants in tests/test_watch_cdc.rs."),
      spacer(80),
      testTable([
        ["C-01", "INSERT propagates to ES",        "Insert a new product; it appears in ES within 5s",                        "INSERT INTO products",    "New doc found in ES with correct fields"],
        ["C-02", "UPDATE propagates to ES",        "Update a product's price; ES reflects the new price within 5s",           "UPDATE products SET price","price == 99.99 in ES"],
        ["C-03", "DELETE propagates to ES",        "Hard-delete a product row; ES no longer returns the document within 5s", "DELETE FROM products",    "es_get returns None"],
        ["C-04", "Rapid mutations settle",         "10 successive updates; ES eventually shows the final value",              "10× UPDATE in loop",      "stock == 90 in ES"],
      ]),
      spacer(),

      // ── 7. Test cases: GraphQL ────────────────────────────────────────
      h1("7. Test Suite: GraphQL Server (test_graphql.rs)"),
      para("Tests for the esync serve command. Each test spawns the Axum server on port 4001 as a background task and makes real HTTP requests to the GraphQL endpoint."),
      spacer(80),
      testTable([
        ["G-01", "list_product returns all rows",     "Query with limit:20 returns all 5 seeded products",                  "list_product(limit:20)", "5 items, no GraphQL errors"],
        ["G-02", "Pagination: limit + offset",        "Three pages of 2/2/1 cover all 5 products with no duplicates",       "limit:2 offset:0,2,4",   "Correct counts, 5 unique IDs total"],
        ["G-03", "Search filters by ILIKE",           "search:\"Widget\" returns only rows whose name contains 'widget'",   "list_product(search:…)", "Every result name contains 'widget'"],
        ["G-04", "Search with no results",            "A nonsense search term returns an empty array, not an error",         "search:\"xyzzy…\"",      "Empty array, no error"],
        ["G-05", "get_product by ID",                 "Fetching PRODUCT_1 returns exact field values from the seed data",   "get_product(id:…)",      "name=Alpha Widget, price=9.99"],
        ["G-06", "get_product not found",             "Querying a non-existent UUID returns null, not an error",            "get_product(id:unknown)","data.get_product is null"],
        ["G-07", "Order list excludes soft-deleted",  "Soft-deleting ORDER_1 causes it to disappear from list_order",       "deleted_at = NOW()",     "2 orders, ORDER_1 absent"],
        ["G-08", "Numeric fields are numbers",        "price and stock must be JSON numbers, not strings",                  "get_product fields",     "price.is_number() = true"],
      ]),
      spacer(),

      // ── 8. Seed data reference ────────────────────────────────────────
      h1("8. Seed Data Reference"),
      para("All integration tests operate on the following fixed dataset. UUIDs are deterministic to allow precise assertions without DB round-trips."),
      spacer(80),

      h2("8.1 Products (5 rows)"),
      new Table({
        width: { size: CONTENT_WIDTH, type: WidthType.DXA },
        columnWidths: [3200, 1600, 1440, 1100, 1000, 1020],
        rows: [
          new TableRow({ tableHeader: true, children: [
            headerCell("UUID suffix", 3200), headerCell("Name",   1600),
            headerCell("Price",       1440), headerCell("Stock",  1100),
            headerCell("Active",      1000), headerCell("created_at", 1020),
          ]}),
          ...[
            ["…000000000001", "Alpha Widget",    "$9.99",   "100", "✓", "2024-01-01"],
            ["…000000000002", "Beta Gizmo",      "$49.99",  "50",  "✓", "2024-02-01"],
            ["…000000000003", "Gamma Doohickey", "$199.00", "10",  "✓", "2024-03-01"],
            ["…000000000004", "Delta Thing",     "$1.00",   "0",   "✗", "2024-04-01"],
            ["…000000000005", "Epsilon Part",    "$5.50",   "200", "✓", "2024-05-01"],
          ].map((r, i) => new TableRow({ children: r.map(v => cell(v, { bg: i % 2 ? stripeBg : null })) }))
        ]
      }),
      spacer(),

      h2("8.2 Orders (3 rows)"),
      new Table({
        width: { size: CONTENT_WIDTH, type: WidthType.DXA },
        columnWidths: [3200, 1800, 1600, 2760],
        rows: [
          new TableRow({ tableHeader: true, children: [
            headerCell("UUID suffix", 3200), headerCell("Status", 1800),
            headerCell("Total",       1600), headerCell("Notes",  2760),
          ]}),
          ...[
            ["…000000000001", "completed", "$59.98",  "Used in soft-delete tests"],
            ["…000000000002", "pending",   "$199.00", "Normal order"],
            ["…000000000003", "cancelled", "$9.99",   "Has promo metadata"],
          ].map((r, i) => new TableRow({ children: r.map(v => cell(v, { bg: i % 2 ? stripeBg : null })) }))
        ]
      }),
      spacer(),

      // ── 9. Test checklist ─────────────────────────────────────────────
      h1("9. Pre-Run Checklist"),
      new Table({
        width: { size: CONTENT_WIDTH, type: WidthType.DXA },
        columnWidths: [720, 8640],
        rows: [
          new TableRow({ tableHeader: true, children: [headerCell("✓", 720), headerCell("Item", 8640)] }),
          ...[
            "docker compose up -d postgres elasticsearch is running",
            "esync_test database created (scripts/test/setup_test_db.sql executed)",
            "seed_test_data() has been called; products table has 5 rows",
            "ES reachable: curl http://localhost:9200/_cluster/health returns green or yellow",
            "No stale test_ indices in ES from a previous run (run_integration_tests.sh cleans these)",
            "ESYNC_CONFIG=esync.test.yaml is set in the environment",
            "Port 4001 is free (esync serve uses it during test_graphql)",
            "cargo build completes without errors",
          ].map((t, i) => new TableRow({
            children: [
              cell("☐", { align: AlignmentType.CENTER, bg: i % 2 ? stripeBg : null }),
              cell(t, { bg: i % 2 ? stripeBg : null }),
            ]
          }))
        ]
      }),
      spacer(),

      // ── 10. Troubleshooting ───────────────────────────────────────────
      h1("10. Troubleshooting"),

      h3("CDC tests time out"),
      para("The watch loop relies on Postgres NOTIFY. Check that the trigger is attached:"),
      para([mono("SELECT trigger_name FROM information_schema.triggers WHERE event_object_table = 'products';")]),
      para("If the trigger is missing, re-run setup_test_db.sql."),

      h3("GraphQL tests fail to connect"),
      para([
        new TextRun({ text: "Port 4001 may already be in use. Kill the stale process with ", size: 20, color: "333333" }),
        mono("lsof -ti:4001 | xargs kill"),
        new TextRun({ text: " and re-run.", size: 20, color: "333333" }),
      ]),

      h3("ES search returns 0 hits immediately after bulk_index"),
      para([
        new TextRun({ text: "Call ", size: 20, color: "333333" }),
        mono("es_refresh(index)"),
        new TextRun({ text: " in the test before asserting on search results. ES does not make documents visible until the next refresh cycle.", size: 20, color: "333333" }),
      ]),

      h3("test_index_command: row count mismatch"),
      para([
        new TextRun({ text: "Another test left extra rows. Call ", size: 20, color: "333333" }),
        mono("reseed(&pool).await?"),
        new TextRun({ text: " at the start of any test that queries by count.", size: 20, color: "333333" }),
      ]),

      h3("CI: Elasticsearch health check times out"),
      para("The ES health check in the CI workflow retries for 60 seconds. If the container takes longer to start (common with < 4 GB RAM runners), increase MAX in the wait loop in the GitHub Actions YAML."),
      spacer(),

    ]
  }]
});

Packer.toBuffer(doc).then(buffer => {
  fs.writeFileSync('/mnt/user-data/outputs/esync_integration_test_plan.docx', buffer);
  console.log('Written: esync_integration_test_plan.docx');
});
