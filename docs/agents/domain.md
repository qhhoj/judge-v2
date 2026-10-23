# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`GLOSSARY.md`** at the repo root.
- **`docs/decisions.md`**: the decisions log, one line per decision (what and why). Read the entries that touch the area you're about to work in. It plays the role ADRs play elsewhere: there is no `docs/adr/`.

If either file doesn't exist, **proceed silently**. Don't flag its absence; don't suggest creating it upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved. A new decision is one line appended to `docs/decisions.md` in its existing style, not a separate ADR file.

## File structure

Single-context repo:

```
/
├── GLOSSARY.md
└── docs/
    └── decisions.md
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `GLOSSARY.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag decision conflicts

If your output contradicts an entry in `docs/decisions.md`, surface it explicitly rather than silently overriding, citing the entry's date and wording:

> _Contradicts the YYYY-MM-DD entry "…", but worth reopening because…_
