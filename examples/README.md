# esync Examples

Ready-to-run examples for different domains and modules.

| Directory | Description |
|---|---|
| [`sap-mm/`](sap-mm/) | SAP MM (Material Management) — material master, vendor, stock, goods movements |

## Adding your own example

1. Create a subdirectory: `examples/your-module/`
2. Add `esync-your-module.yaml` — entity + relation + search config
3. Add `scripts/init.sql` — table DDL, CDC triggers, seed data
4. Add `docker-compose.yml` — copy and adjust from an existing example
5. Add `README.md` — GraphQL query examples

No Rust code changes are ever needed.
