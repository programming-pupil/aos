# AOS NL2SQL Reference Binding

Strategy contract for user-bound reference files in Data Exploration. References
can be README files, SQL demos, Python scripts, notebooks exported as text,
metric dictionaries, or any UTF-8 text that explains business query patterns.
Storage, upload validation, SQL safety, datasource permissions, and query
execution stay in Rust.

## Contract

- Treat selected references as trusted examples for business meaning, not as executable truth.
- Use references to understand metric formulas, Hive/SQL idioms, joins, filters, table aliases, and internal vocabulary.
- Live schema, datasource permissions, SQL safety rules, and explicit user instructions override references.
- Do not copy a reference query blindly. Adapt it to the current question and live schema.
- Cite reference IDs or snippets in trace metadata when they affect SQL generation.
- If references conflict with schema or policy, explain the conflict and prefer safe SQL.
- If no relevant reference is found, continue with normal NL2SQL behavior.

## Good Reference Examples

- ROI query templates with revenue and cost formula.
- Business metric README files.
- Existing Hive SQL reports.
- Python scripts that show table joins or API-derived fields.
- Glossaries explaining app, campaign, channel, or country naming.
