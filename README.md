# Course Stack MCP

MCP server for the [CourseStack](https://app.coursestack.com) API. Exposes every
`courses` / `content` / `enrollments` / `students` / `event-registrations` /
`bundle-enrollments` endpoint as an MCP tool, so Claude Code, Claude Desktop,
Codex, or any other MCP client can manage a CourseStack instance directly.

Written in Rust. Single static-ish binary, no runtime, no config file — one
API key in an environment variable.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/JoasASantos/Course-Stack-MCP/main/install.sh | bash
```

Downloads a prebuilt binary for macOS/Linux (`arm64`/`x86_64`) from the latest
GitHub release into `~/.local/bin`, or builds from source with `cargo` if no
matching release exists yet.

Manual build:

```bash
git clone https://github.com/JoasASantos/Course-Stack-MCP.git
cd Course-Stack-MCP
cargo build --release
# binary at target/release/coursestack-mcp
```

## Get an API key

CourseStack API keys (`sk_...`) are managed inside the CourseStack application.
Authentication is HTTP Basic with the key as the username and no password —
handled internally, you just provide the key.

## Configure

### Claude Code

```bash
claude mcp add coursestack --scope user \
  --env COURSESTACK_API_KEY=sk_your_key \
  -- ~/.local/bin/coursestack-mcp
```

Or generate the same command:

```bash
coursestack-mcp config claude-code
```

### Claude Desktop

Add to `claude_desktop_config.json`
(macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "coursestack": {
      "command": "/Users/you/.local/bin/coursestack-mcp",
      "args": [],
      "env": { "COURSESTACK_API_KEY": "sk_your_key" }
    }
  }
}
```

(`coursestack-mcp config claude-desktop` prints this with the resolved binary
path filled in.)

### Codex CLI

`~/.codex/config.toml`:

```toml
[mcp_servers.coursestack]
command = "/Users/you/.local/bin/coursestack-mcp"
args = []
env = { COURSESTACK_API_KEY = "sk_your_key" }
```

(`coursestack-mcp config codex`)

## Verify

```bash
export COURSESTACK_API_KEY=sk_your_key
coursestack-mcp doctor
```

Prints the resolved config and does a live `GET /api/students` call.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `COURSESTACK_API_KEY` | — | **Required.** Secret key. |
| `COURSESTACK_BASE_URL` | `https://app.coursestack.com` | API host, for self-hosted/staging instances. |
| `COURSESTACK_AUTH_MODE` | `basic` | `basic` (key as Basic-auth username, per CourseStack docs) or `bearer`. |
| `COURSESTACK_READ_ONLY` | `0` | `1` refuses every non-`GET` call — safe mode for exploration. |
| `COURSESTACK_INCLUDE_DEPRECATED` | `0` | `1` also exposes the deprecated `/api/enrollments` tools. |
| `COURSESTACK_TIMEOUT_SECS` | `60` | HTTP request timeout. |
| `COURSESTACK_OPENAPI` | *(embedded)* | Path to an OpenAPI document, to pick up API changes without a rebuild. |
| `COURSESTACK_MAX_RETRIES` | `2` | Extra attempts on `429` (any method) or retryable `5xx` (`GET` only), with exponential backoff honoring `Retry-After`. |
| `COURSESTACK_RETRY_BASE_MS` | `300` | Backoff base; doubles per attempt, capped at 8s (or the server's `Retry-After`, capped at 30s). |
| `COURSESTACK_MAX_PAGES` | `20` | Default cap when a tool is called with `all_pages: true`. |
| `COURSESTACK_STRICT_VALIDATION` | `1` | `0` skips the client-side pre-flight checks (below) and lets CourseStack be the only validator. |
| `COURSESTACK_DEBUG` | `0` | `1` logs method/URL/status/timing (never the key) to stderr. |

## What's exposed

Tools are generated straight from CourseStack's OpenAPI document (vendored at
`spec/coursestack.min.json`), one per operation, named after its
`operationId` (`courseEnrollments.create` → `course_enrollments_create`).
Covers:

- `courses` — retrieve
- `content`, `content.chapters`, `content.lessons` — full CRUD + reordering
- `content.files`, `content.jupyter` — file records + presigned upload/download
- `content.networks`, `content.systems` — lab environment configuration
- `courseEnrollments`, `bundleEnrollments`, `eventRegistrations` — enroll/list/update/remove
- `students` — list/search, retrieve

Run `coursestack-mcp tools` to print the full list with HTTP method + path.

Four extra tools are always available:

- `coursestack_request` — call any path directly (escape hatch for endpoints
  added to the API before this tool catalog is regenerated).
- `coursestack_upload_file` — PUT a local file to a presigned URL returned by
  the `*_files_create` / `*_jupyter_create` tools.
- `students_find_by_email` — `students_list`'s `search` is a fuzzy match;
  this filters down to the exact match(es) so an agent doesn't need a
  follow-up round-trip to disambiguate.
- `content_tree` — fetches a content item with every chapter and lesson
  under it (auto-paginating both), lessons grouped by chapter, in one call.
  Replaces `content_retrieve` + `content_chapters_list` + `content_lessons_list`.

## Uploading a file

File uploads are two calls, matching the API's own two-step design:

1. `content_files_create` with the content metadata → returns a presigned URL.
2. `coursestack_upload_file` with that URL and a local `file_path`.

## Pagination

The 8 list endpoints that expose a `next_key` cursor (`content_list`,
`content_chapters_list`, `content_lessons_list`, `students_list`,
`course_enrollments_list`, `bundle_enrollments_list`,
`event_registrations_list`, and deprecated `enrollments_list`) accept two
extra arguments:

- `all_pages: true` — follow the cursor automatically, merging every page's
  array fields into one result.
- `max_pages` — cap on pages fetched (default `COURSESTACK_MAX_PAGES`, 20).

The merged result adds `pages_fetched` and, if the cap was hit before the API
ran out of pages, `truncated: true` — check for that rather than assuming
you got everything back.

## Reliability

Requests retry automatically: `429` on any method, and retryable `5xx`
(`500`/`502`/`503`/`504`) on `GET` only — writes aren't retried on `5xx`
since a partial failure on the server side can't be told apart from success.
Backoff is exponential from `COURSESTACK_RETRY_BASE_MS`, or follows the
server's `Retry-After` header when present.

## Client-side validation

Every call is checked against the operation's OpenAPI schema before it goes
out: required fields, UUID/email formats, enum membership, string/array
length bounds. This is deliberately shallow — schemas too complex to check
safely (the `oneOf` discriminated unions used for lesson content trees and
network configs) are skipped rather than guessed, so a payload the API would
accept is never rejected client-side. CourseStack's own response is always
the final word; this just catches typos before a round-trip.

Set `COURSESTACK_STRICT_VALIDATION=0` to turn it off entirely.

## Troubleshooting

`coursestack-mcp doctor` probes several resources, not just one — API keys
can be scoped, so a `403` on `/api/students` doesn't mean the key is broken,
just that it lacks that one permission. It only fails if every probe comes
back `401` (the key itself is rejected).

Set `COURSESTACK_DEBUG=1` to log every request's method, URL, status, and
timing (never the key) to stderr — useful when a tool call fails and you
need to see what was actually sent.

## Development

```bash
cargo test              # unit tests (spec parsing, tool generation, protocol)
cargo clippy --all-targets -- -D warnings
cargo fmt
coursestack-mcp tools   # sanity-check the generated tool catalog
```

To refresh the vendored spec after a CourseStack API change:

```bash
curl -fsSL https://app.coursestack.com/api/openapi.json -o spec/coursestack.openapi.json
# trim to the request-relevant fields and write spec/coursestack.min.json
```

## License

MIT — see [LICENSE](LICENSE).
