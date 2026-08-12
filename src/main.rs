mod client;
mod config;
mod server;
mod spec;
mod validate;

use serde_json::Map;

use client::CourseStack;
use config::{AuthMode, Config, DEFAULT_BASE_URL};
use server::Server;
use spec::{build_tools, load_spec, Method};

const HELP: &str = r#"coursestack-mcp — MCP server for the CourseStack API

USAGE:
    coursestack-mcp [COMMAND]

COMMANDS:
    (none)              Run the MCP server on stdio (what MCP clients launch)
    tools               List every exposed tool with its HTTP method and path
    doctor              Verify configuration and credentials against the API
    config [CLIENT]     Print client configuration: claude-code | claude-desktop | codex
    --version, -V       Print the version
    --help, -h          Print this help

ENVIRONMENT:
    COURSESTACK_API_KEY          Required. Secret key (starts with `sk_`)
    COURSESTACK_BASE_URL         Default https://app.coursestack.com
    COURSESTACK_AUTH_MODE        `basic` (default, key as basic-auth username) or `bearer`
    COURSESTACK_READ_ONLY        `1` to refuse every non-GET call
    COURSESTACK_INCLUDE_DEPRECATED  `1` to also expose /api/enrollments (deprecated)
    COURSESTACK_TIMEOUT_SECS     Request timeout, default 60
    COURSESTACK_OPENAPI          Path to an OpenAPI document overriding the embedded one
    COURSESTACK_MAX_RETRIES      Extra attempts on 429/retryable 5xx, default 2
    COURSESTACK_RETRY_BASE_MS    Backoff base for retries, default 300
    COURSESTACK_MAX_PAGES        Default page cap for all_pages=true, default 20
    COURSESTACK_STRICT_VALIDATION  `0` to skip client-side pre-flight checks, default on
    COURSESTACK_DEBUG            `1` to log method/URL/status/timing to stderr
"#;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");

    let code = match command {
        "" => run_server(),
        "--help" | "-h" | "help" => {
            print!("{HELP}");
            Ok(())
        }
        "--version" | "-V" | "version" => {
            println!("coursestack-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "tools" => list_tools(),
        "doctor" => doctor(),
        "config" => {
            print_config(args.get(1).map(String::as_str).unwrap_or("claude-code"));
            Ok(())
        }
        other => Err(format!(
            "unknown command `{other}`. Run `coursestack-mcp --help`."
        )),
    };

    if let Err(message) = code {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn load(cfg: &Config) -> Result<Vec<spec::ToolDef>, String> {
    let document = load_spec(cfg.spec_path.as_deref())?;
    Ok(build_tools(&document, cfg.include_deprecated))
}

fn run_server() -> Result<(), String> {
    let cfg = Config::from_env()?;
    let tools = load(&cfg)?;
    eprintln!(
        "coursestack-mcp {} ready: {} tools, base {}{}",
        env!("CARGO_PKG_VERSION"),
        tools.len() + server::builtin_tool_descriptors().len(),
        cfg.base_url,
        if cfg.read_only { ", read-only" } else { "" }
    );
    let client = CourseStack::new(cfg)?;
    Server::new(client, tools).serve_stdio()
}

fn list_tools() -> Result<(), String> {
    // Listing must work without credentials, so the spec is read directly.
    let include_deprecated = matches!(
        std::env::var("COURSESTACK_INCLUDE_DEPRECATED").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    let path = std::env::var("COURSESTACK_OPENAPI").ok();
    let document = load_spec(path.as_deref())?;
    let tools = build_tools(&document, include_deprecated);

    for tool in &tools {
        println!(
            "{:<38} {:<6} {}",
            tool.name,
            tool.method.as_str(),
            tool.path
        );
    }
    let builtins = server::builtin_tool_descriptors();
    for b in &builtins {
        let name = b["name"].as_str().unwrap_or("?");
        let hint = if b["annotations"]["readOnlyHint"].as_bool().unwrap_or(false) {
            "(read-only helper)"
        } else {
            "(escape hatch / helper)"
        };
        println!("{name:<38} {:<6} {hint}", "-");
    }
    println!("\n{} tools", tools.len() + builtins.len());
    Ok(())
}

fn doctor() -> Result<(), String> {
    let cfg = Config::from_env()?;
    println!("base URL      {}", cfg.base_url);
    println!(
        "auth          {} (key {}…, {} chars)",
        match cfg.auth_mode {
            AuthMode::Basic => "HTTP Basic, key as username",
            AuthMode::Bearer => "Bearer token",
        },
        cfg.api_key.chars().take(5).collect::<String>(),
        cfg.api_key.len()
    );
    println!("read-only     {}", cfg.read_only);
    println!(
        "retries       {} attempts, {}ms base backoff",
        cfg.max_retries, cfg.retry_base_ms
    );
    println!(
        "max pages     {} (all_pages=true default cap)",
        cfg.max_pages
    );

    let tools = load(&cfg)?;
    println!(
        "tools         {}",
        tools.len() + server::builtin_tool_descriptors().len()
    );

    let client = CourseStack::new(cfg)?;

    // Probe several resources rather than just one: CourseStack keys can be
    // scoped, so one endpoint (403) doesn't tell you about the rest.
    const PROBES: &[(&str, &str)] = &[
        ("/api/students", "students"),
        ("/api/content", "content"),
        ("/api/course-enrollments", "course enrollments"),
        ("/api/bundle-enrollments", "bundle enrollments"),
        ("/api/event-registrations", "event registrations"),
    ];
    println!();
    let mut unauthenticated = 0;
    for (path, label) in PROBES {
        let response = client.call(Method::Get, path, &Map::new(), None)?;
        let verdict = match response.status {
            200..=299 => "ok",
            400 => "ok (auth passed; endpoint needs query params this probe didn't send)",
            401 => {
                unauthenticated += 1;
                "INVALID CREDENTIALS"
            }
            403 => "forbidden (key lacks permission for this resource)",
            other => return Err(format!("unexpected HTTP {other} probing {path}")),
        };
        println!(
            "GET {path:<28} -> HTTP {:<3} {verdict} [{label}]",
            response.status
        );
    }

    if unauthenticated == PROBES.len() {
        return Err("every probe returned 401: the API key itself is rejected".to_string());
    }
    println!("\nconnection healthy (key is recognized by CourseStack)");
    Ok(())
}

fn print_config(client: &str) {
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "coursestack-mcp".to_string());

    match client {
        "claude-code" => {
            println!("# Claude Code — run once:");
            println!(
                "claude mcp add coursestack --scope user --env COURSESTACK_API_KEY=sk_your_key -- {binary}"
            );
        }
        "claude-desktop" => {
            println!("# Claude Desktop — ~/Library/Application Support/Claude/claude_desktop_config.json");
            println!("{}", desktop_snippet(&binary));
        }
        "codex" => {
            println!("# Codex CLI — ~/.codex/config.toml");
            println!("[mcp_servers.coursestack]");
            println!("command = \"{binary}\"");
            println!("args = []");
            println!("env = {{ COURSESTACK_API_KEY = \"sk_your_key\" }}");
        }
        other => {
            eprintln!("unknown client `{other}`; expected claude-code, claude-desktop or codex");
        }
    }
    println!("\n# Base URL defaults to {DEFAULT_BASE_URL} (override with COURSESTACK_BASE_URL).");
}

fn desktop_snippet(binary: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "coursestack": {
                "command": binary,
                "args": [],
                "env": { "COURSESTACK_API_KEY": "sk_your_key" }
            }
        }
    }))
    .unwrap_or_default()
}
