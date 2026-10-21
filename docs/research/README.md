# Harness research

Every rich session adapter in `src/sources/` is based on an upstream investigation of the harness. The reports in this folder are the evidence for the storage paths, the record formats and the limits of each adapter.

Each report contains the same fields:

| Field | Meaning |
| --- | --- |
| `storage.paths` | The exact locations of the local activity store, with the upstream file that defines them |
| `storage.format` | The shape of the store: JSONL, JSON document, Markdown or SQLite |
| `storage.append_semantics` | Whether the store is appended live, rewritten in place, or written to a database |
| `fields.*` | Which of the project, model, file, activity, phase and timestamps are recoverable |
| `sample_record` | A redacted or clearly labelled synthetic sample of a record |
| `discodex_plan` | The integration that was implemented |
| `logo_url` | The Discord artwork, verified with an HTTP request |
| `evidence` | The upstream URLs, with file and line references where possible |
| `risks` | The format churn that the adapter has to survive |

## Rules the research followed

- The research is upstream only. No file of a harness installation was read to derive a path or a format; the local installations were only used afterwards to check that the adapters work against real session files.
- Every claim carries a URL. Anything that could not be verified is marked `UNVERIFIED` instead of being guessed.
- A harness that keeps its data in an encrypted file, a proprietary database or only on a server is reported as not viable for rich tracking, even when the product is popular.

## Keeping the reports current

The stores of these harnesses change. When an adapter stops reporting activity, compare the report with the current upstream release and update the report together with the adapter.
