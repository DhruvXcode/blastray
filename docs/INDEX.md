# Index

The index is a small code graph, not a general-purpose database.

Mission 1 entities:

- files
- symbols

Symbol identity is `repo-relative/path.ts::SymbolName`; methods use
`repo-relative/path.ts::ClassName.methodName`.

Mission 1 relationships:

- `DEFINES`
- `IMPORTS`
- `CALLS`

Every attempted import or call is resolved, ambiguous, or unresolved. Only
resolved relationships become graph edges.
