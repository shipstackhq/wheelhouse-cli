---
description: Map this repo's database schema into the Wheelhouse data layer (tables, columns, foreign keys, indexes, enums)
argument-hint: "[PROJECT_KEY]"
---

Map the database schema into the data layer of Wheelhouse project `$ARGUMENTS` (or `project` from `wheelhouse.json`).

1. Find the schema's source of truth: ORM schema files (Drizzle, Prisma, TypeORM, SQLAlchemy, ActiveRecord…), SQL migrations or DDL. Prefer the ORM schema and cross-check the latest migrations.
2. Call `get_schema` to see what's mapped; update rather than duplicate.
3. For every table, view and enum call `upsert_table` with all columns in schema order (type as written, pk, nullable, unique, default, `references: "table.column"` with `onDelete`), important indexes, a one-line summary of what a row represents, gotchas, anchors (`schema-file#table` plus its migration), `implements` (the concept it stores and the system database), and `described_sha`.
4. Re-check the `unresolved` lists; every foreign-key target should end up mapped.
5. Add glossary terms for domain words used as table/column names, and ADRs for notable modelling choices.
Report how many tables and relationships you mapped.
