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

Two extra tools are always available:

- `coursestack_request` — call any path directly (escape hatch for endpoints
  added to the API before this tool catalog is regenerated).
- `coursestack_upload_file` — PUT a local file to a presigned URL returned by
  the `*_files_create` / `*_jupyter_create` tools.

## Uploading a file

File uploads are two calls, matching the API's own two-step design:

1. `content_files_create` with the content metadata → returns a presigned URL.
2. `coursestack_upload_file` with that URL and a local `file_path`.

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
