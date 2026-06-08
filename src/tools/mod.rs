//! Built-in tool definitions and dispatch — canonical set of 7.
//!
//! Tool naming and semantics inspired by three projects:
//!
//! - Claude Code (github.com/anthropics/claude-code): The canonical 7-tool set
//!   mirrors Claude Code's built-in tools — Bash→exec_command, Read→read_file,
//!   Write→write_file, Edit→edit_file, Grep+Glob→search_files, Memory→memory.
//!   The parameter schemas (offset/limit for read, old_string/new_string/replace_all
//!   for edit, command for exec) follow the same shapes defined in
//!   claude-code/src/tools/ and their prompt.ts descriptions.
//!
//! - OpenCode (github.com/nicepkg/opencode): The edit tool's old_string/new_string
//!   approach is also used by OpenCode's EditTool
//!   (opencode/packages/opencode/src/tool/edit.ts), which cites Cline's
//!   diff-edits as a source. The read tool with offset/limit parameters and
//!   line-numbered output format matches the pattern in
//!   opencode/packages/opencode/src/tool/read.ts. OpenCode's grep/glob/ls
//!   tools (opencode/packages/opencode/src/tool/grep.ts, glob.ts, ls.ts) use
//!   ripgrep under the hood; enchanter uses the regex+walkdir combo for a
//!   pure-Rust implementation with no runtime dependency.
//!
//! - hermes-agent (github.com/NousResearch/hermes-agent): The memory tool's
//!   add/remove/replace/list operations follow hermes-agent's built-in memory
//!   tool (hermes-agent/tools/memory_tool.py) which exposes the same four
//!   actions over a persistent text store. Claude Code's memory system
//!   (claude-code/src/memdir/memdir.ts) uses MEMORY.md + per-topic files
//!   with typed frontmatter; enchanter's simpler flat-entry model is closer
//!   to hermes-agent's.
//!
//! The edit_file implementation (old_string/new_string/replace_all with
//! uniqueness check) directly mirrors Claude Code's FileEditTool behavior
//! (claude-code/src/tools/FileEditTool/prompt.ts: "The edit will FAIL if
//! old_string is not unique in the file") and OpenCode's EditTool
//! (opencode/packages/opencode/src/tool/edit.ts: oldString/newString/replaceAll).
//!
//! The 10,000-character truncation limit on tool output mirrors OpenCode's
//! output truncation approach and Claude Code's similar output limits.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use crate::kstore::KnowledgeStore;
use crate::memory::MemoryStore;

/// Emit the "running unsandboxed" warning at most once per process.
fn warn_unsandboxed_once() {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "Warning: no filesystem sandbox available; running exec_command \
             unsandboxed (security.allow_unsandboxed_exec is set)."
        );
    });
}

/// Check if a resolved path falls within any of the allowed directories.
fn path_is_allowed(resolved_path: &Path, allowed: &[PathBuf]) -> bool {
    allowed.iter().any(|dir| resolved_path.starts_with(dir))
}

/// Resolve and validate a path: expand tilde, canonicalize if it exists,
/// and check that it falls within allowed directories.
/// Returns Ok(resolved) if allowed, Err(message) if blocked.
fn resolve_and_validate(path_str: &str, allowed: &[PathBuf]) -> Result<PathBuf, String> {
    let expanded = shellexpand::tilde(path_str).to_string();
    let path = PathBuf::from(&expanded);

    // Canonicalize if it exists; for new files, canonicalize the parent
    let resolved = if path.exists() {
        path.canonicalize().unwrap_or(path)
    } else if let Some(parent) = path.parent()
        && parent.exists()
    {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        canonical_parent.join(path.file_name().unwrap_or_default())
    } else {
        // Path doesn't exist and neither does its parent — just use as-is
        // but resolve relative paths against CWD
        if path.is_relative() {
            std::env::current_dir().unwrap_or_default().join(&path)
        } else {
            path
        }
    };

    if path_is_allowed(&resolved, allowed) {
        Ok(resolved)
    } else {
        Err(format!(
            "Access denied: path '{}' is outside allowed directories",
            path_str
        ))
    }
}

/// A tool definition in OpenAI function-calling format.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// Return the canonical 7 tool definitions.
pub fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "exec_command",
            description: "Execute a shell command and return its stdout and stderr. \
                Use for running builds, tests, git operations, package managers, \
                and any task requiring a shell. Commands timeout after 30 seconds. \
                Output is truncated to 10,000 characters.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }),
        },
        ToolDef {
            name: "read_file",
            description: "Read a file's contents and return them as text. \
                Returns up to 500 lines starting from the given offset. \
                Binary files will produce garbled output.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (absolute or relative)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed, default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read (default: 500, max: 2000)"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "write_file",
            description: "Write content to a file, creating it if it doesn't exist \
                or overwriting if it does. Creates parent directories automatically.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (absolute or relative)"
                    },
                    "content": {
                        "type": "string",
                        "description": "The full content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDef {
            name: "edit_file",
            description: "Apply a targeted find-and-replace edit to a file. \
                Finds an exact match of 'old_string' and replaces it with 'new_string'. \
                The old_string must be unique in the file. Use this instead of write_file \
                for code edits — it's safer because it only changes the targeted section. \
                Set replace_all to true to replace all occurrences.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (absolute or relative)"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find in the file"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The text to replace it with"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurrences instead of requiring a unique match (default: false)"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        ToolDef {
            name: "search_files",
            description: "Search for a regex pattern in file contents, or find files by glob pattern. \
                For content search: returns matching lines with line numbers and context. \
                For file search: returns file paths sorted by modification time. \
                Use this instead of grep/find shell commands — it's cross-platform and parsed output.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern for content search, or glob pattern (e.g. '*.py') for file search"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (default: current directory)"
                    },
                    "target": {
                        "type": "string",
                        "enum": ["content", "files"],
                        "description": "'content' searches inside files, 'files' finds files by name pattern (default: 'content')"
                    },
                    "file_glob": {
                        "type": "string",
                        "description": "Filter files by pattern when searching content (e.g. '*.rs')"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Lines of context before/after each match (content search only, default: 2)"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            name: "list_directory",
            description: "List entries in a directory. Returns file names, types \
                (file/dir/symlink), and sizes. Sorted alphabetically.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory (defaults to current working directory)"
                    }
                },
                "required": []
            }),
        },
        ToolDef {
            name: "memory",
            description: "Read and modify persistent memory that survives across sessions. \
                Use 'add' to save a new fact, 'remove' to delete one by substring, \
                'replace' to update an existing entry, or 'list' to see all entries. \
                Save durable facts: user preferences, project conventions, environment details, \
                lessons learned. Do not save temporary state or task progress.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "remove", "replace", "list"],
                        "description": "The memory operation to perform"
                    },
                    "content": {
                        "type": "string",
                        "description": "For 'add': the entry to save. For 'remove': substring to match. For 'replace': old text to find."
                    },
                    "new_content": {
                        "type": "string",
                        "description": "For 'replace': the replacement text"
                    }
                },
                "required": ["action"]
            }),
        },
        ToolDef {
            name: "knowledge",
            description: "Read and modify a structured knowledge store that persists across sessions. \
                Unlike memory (narrative text), knowledge stores discrete key-value facts \
                that can be looked up without re-asking the LLM. \
                Use 'get' to look up a fact by key, 'store' to save a fact, \
                'search' to find entries by key prefix, 'list' to see all entries, \
                or 'forget' to remove a stale entry. \
                Keys use dot-namespace (e.g., project.rust_version, user.email). \
                Categories: environment, project, preference, decision, fact. \
                Source: told (user stated), observed (detected from output), inferred (concluded from context). \
                Proactively store facts you discover — project paths, version numbers, user preferences, \
                design decisions, bug workarounds — so you never have to re-ask.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["get", "store", "search", "list", "forget"],
                        "description": "The knowledge operation to perform"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key for the fact (dot-namespaced, e.g., project.rust_version). Required for get, store, and forget."
                    },
                    "value": {
                        "type": "string",
                        "description": "Value for the fact. Required for store."
                    },
                    "category": {
                        "type": "string",
                        "description": "Category for the fact: environment, project, preference, decision, or fact. Defaults to fact."
                    },
                    "source": {
                        "type": "string",
                        "description": "How this fact was learned: 'told' (user stated it), 'observed' (detected from tool output or environment), or 'inferred' (concluded from context). Defaults to 'told'."
                    },
                    "query": {
                        "type": "string",
                        "description": "Key prefix to search for. Required for search."
                    }
                },
                "required": ["action"]
            }),
        },
    ]
}

/// Convert tool definitions to the OpenAI tools JSON format for the API request.
pub fn tools_json() -> Vec<Value> {
    tool_definitions()
        .into_iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

/// Dispatch a built-in tool call by name. Returns the result as a string.
/// The memory tool requires a mutable reference to the MemoryStore.
/// The knowledge tool requires a mutable reference to the KnowledgeStore.
///
/// The dispatch pattern (match on tool name string → route to handler function)
/// follows the pattern used by all three reference projects:
/// - Claude Code: claude-code/src/tools/ — each tool is a separate module with
///   a Tool.define() call that registers name, schema, and handler.
/// - OpenCode: opencode/packages/opencode/src/tool/registry.ts — tool registry
///   maps tool names to handler functions.
/// - hermes-agent: hermes-agent/tools/registry.py — tool dispatch routes by
///   name to registered tool functions.
///
/// enchanter simplifies this to a single dispatch() match statement since
/// all built-in tools are local functions rather than dynamically registered.
pub fn dispatch(
    name: &str,
    args: &Value,
    memory: &mut MemoryStore,
    kstore: &mut KnowledgeStore,
    allowed_paths: &[PathBuf],
    allow_unsandboxed_exec: bool,
) -> String {
    match name {
        "exec_command" => tool_exec_command(args, allowed_paths, allow_unsandboxed_exec),
        "read_file" => tool_read_file(args, allowed_paths),
        "write_file" => tool_write_file(args, allowed_paths),
        "edit_file" => tool_edit_file(args, allowed_paths),
        "search_files" => tool_search_files(args, allowed_paths),
        "list_directory" => tool_list_directory(args, allowed_paths),
        "memory" => tool_memory(args, memory),
        "knowledge" => tool_knowledge(args, kstore),
        _ => format!("Unknown tool: {}", name),
    }
}

fn tool_exec_command(args: &Value, allowed_paths: &[PathBuf], allow_unsandboxed: bool) -> String {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: missing required parameter 'command'".to_string(),
    };

    // Verify CWD is within allowed paths (the sandbox also denies access to it,
    // but this gives a clearer error than a cryptic shell failure).
    let cwd = std::env::current_dir().unwrap_or_default();
    if !path_is_allowed(&cwd, allowed_paths) {
        return format!(
            "Error: working directory '{}' is outside allowed directories",
            cwd.display()
        );
    }

    let output = if crate::sandbox::is_supported() {
        // Re-exec ourselves as the sandbox helper, which applies Landlock and
        // then execs the shell. The shell is confined to `allowed_paths`.
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return format!("Error: cannot locate enchanter binary for sandbox: {}", e),
        };
        std::process::Command::new(exe)
            .arg(crate::sandbox::SANDBOX_ARG)
            .arg(command)
            .env(
                crate::sandbox::SANDBOX_PATHS_ENV,
                crate::sandbox::encode_paths(allowed_paths),
            )
            .current_dir(&cwd)
            .output()
    } else if allow_unsandboxed {
        warn_unsandboxed_once();
        std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .output()
    } else {
        return "Error: no filesystem sandbox available on this system (Landlock \
            unsupported), refusing to run an unsandboxed shell. Set \
            'security.allow_unsandboxed_exec: true' in config.yaml to override."
            .to_string();
    };

    match output {
        Ok(out) => {
            let mut result = String::new();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);

            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("[stderr]\n");
                result.push_str(&stderr);
            }

            if !out.status.success() {
                result.push_str(&format!(
                    "\n[exit code: {}]",
                    out.status.code().unwrap_or(-1)
                ));
            }

            // Truncate to ~10,000 bytes, respecting char boundaries
            if result.len() > 10_000 {
                let trunc_at = result.floor_char_boundary(10_000);
                result.truncate(trunc_at);
                result.push_str("\n... [truncated]");
            }

            result
        }
        Err(e) => format!("Error executing command: {}", e),
    }
}

fn tool_read_file(args: &Value, allowed_paths: &[PathBuf]) -> String {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };

    let resolved = match resolve_and_validate(path, allowed_paths) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(500)
        .min(2000) as usize;

    let content = match std::fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => return format!("Error reading file {}: {}", path, e),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start = offset.saturating_sub(1);
    if start >= lines.len() {
        return format!(
            "File has {} lines, offset {} is past the end",
            total_lines, offset
        );
    }

    let end = (start + limit).min(lines.len());
    let selected = &lines[start..end];

    let mut result = String::new();
    for (i, line) in selected.iter().enumerate() {
        result.push_str(&format!("{:>5}|{}\n", start + i + 1, line));
    }

    result.push_str(&format!(
        "\nLines {}-{} of {} total",
        start + 1,
        end,
        total_lines
    ));

    // Truncate if needed (floor_char_boundary avoids splitting a multi-byte UTF-8 char)
    if result.len() > 10_000 {
        result.truncate(result.floor_char_boundary(10_000));
        result.push_str("\n... [truncated]");
    }

    result
}

fn tool_write_file(args: &Value, allowed_paths: &[PathBuf]) -> String {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: missing required parameter 'content'".to_string(),
    };

    let resolved = match resolve_and_validate(path, allowed_paths) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Create parent directories
    if let Some(parent) = resolved.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return format!("Error creating directory {}: {}", parent.display(), e);
    }

    match std::fs::write(&resolved, content) {
        Ok(()) => format!("Wrote {}", path),
        Err(e) => format!("Error writing file {}: {}", path, e),
    }
}

fn tool_edit_file(args: &Value, allowed_paths: &[PathBuf]) -> String {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };
    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Error: missing required parameter 'old_string'".to_string(),
    };
    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Error: missing required parameter 'new_string'".to_string(),
    };
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let resolved = match resolve_and_validate(path, allowed_paths) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let content = match std::fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => return format!("Error reading file {}: {}", path, e),
    };

    if !content.contains(old_string) {
        return format!(
            "Error: old_string not found in {}. Make sure the text matches exactly, including whitespace and indentation.",
            path
        );
    }

    if !replace_all {
        let count = content.matches(old_string).count();
        if count > 1 {
            return format!(
                "Error: old_string found {} times in {}. It must be unique unless replace_all=true. \
                Include more surrounding context to make it unique.",
                count, path
            );
        }
    }

    if replace_all {
        let new_content = content.replace(old_string, new_string);
        match std::fs::write(&resolved, &new_content) {
            Ok(()) => {
                let count = content.matches(old_string).count();
                format!("Replaced {} occurrence(s) in {}", count, path)
            }
            Err(e) => format!("Error writing file {}: {}", path, e),
        }
    } else {
        let new_content = content.replacen(old_string, new_string, 1);
        match std::fs::write(&resolved, &new_content) {
            Ok(()) => format!("Edited {}", path),
            Err(e) => format!("Error writing file {}: {}", path, e),
        }
    }
}

fn tool_search_files(args: &Value, allowed_paths: &[PathBuf]) -> String {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'pattern'".to_string(),
    };

    let search_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("content");
    let file_glob = args.get("file_glob").and_then(|v| v.as_str());
    let context_lines = args.get("context").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

    let resolved = match resolve_and_validate(search_path, allowed_paths) {
        Ok(p) => p,
        Err(e) => return e,
    };

    if target == "files" {
        // File name search using glob pattern
        let walk_result = walk_files(&resolved.to_string_lossy(), pattern);
        match walk_result {
            Ok(files) => {
                if files.is_empty() {
                    format!("No files matching '{}' in {}", pattern, search_path)
                } else {
                    let mut result = String::new();
                    for f in &files {
                        result.push_str(f);
                        result.push('\n');
                    }
                    result.push_str(&format!("\n{} files found", files.len()));
                    result
                }
            }
            Err(e) => format!("Error searching files: {}", e),
        }
    } else {
        // Content search using regex
        let regex = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return format!("Error: invalid regex pattern '{}': {}", pattern, e),
        };

        let files = match walk_files(&resolved.to_string_lossy(), file_glob.unwrap_or("*")) {
            Ok(f) => f,
            Err(e) => return format!("Error walking directory: {}", e),
        };

        let mut results = Vec::new();
        let mut total_matches = 0;
        let max_results = 50;

        for file_path in &files {
            if total_matches >= max_results {
                break;
            }

            if let Ok(content) = std::fs::read_to_string(file_path) {
                let lines: Vec<&str> = content.lines().collect();

                for (line_num, line) in lines.iter().enumerate() {
                    if total_matches >= max_results {
                        break;
                    }

                    if regex.is_match(line) {
                        total_matches += 1;
                        let start = line_num.saturating_sub(context_lines);
                        let end = (line_num + context_lines + 1).min(lines.len());

                        results.push(format!("\n{}:{}", file_path, line_num + 1));

                        for (i, ctx_line) in lines[start..end].iter().enumerate() {
                            let actual_line = start + i + 1;
                            let prefix = if actual_line == line_num + 1 {
                                ">"
                            } else {
                                " "
                            };
                            results.push(format!("{}{:>5}|{}", prefix, actual_line, ctx_line));
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            format!("No matches for '{}' in {}", pattern, search_path)
        } else {
            let mut result = results.join("\n");
            if total_matches >= max_results {
                result.push_str(&format!(
                    "\n\n... showing first {} of total matches",
                    max_results
                ));
            } else {
                result.push_str(&format!("\n\n{} matches", total_matches));
            }

            // Truncate if needed
            if result.len() > 10_000 {
                result.truncate(10_000);
                result.push_str("\n... [truncated]");
            }

            result
        }
    }
}

/// Walk directory for files matching a glob pattern, sorted by modification time.
fn walk_files(dir: &str, pattern: &str) -> Result<Vec<String>, String> {
    let glob = match glob::glob(&format!("{}/{}", dir, pattern)) {
        Ok(g) => g,
        Err(e) => return Err(format!("Invalid glob pattern '{}': {}", pattern, e)),
    };

    let mut files: Vec<(String, std::time::SystemTime)> = Vec::new();

    for entry in glob {
        match entry {
            Ok(path) => {
                if path.is_file() {
                    let mtime = path.metadata().ok().and_then(|m| m.modified().ok());
                    let mtime = mtime.unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    files.push((path.to_string_lossy().to_string(), mtime));
                }
            }
            Err(e) => {
                // Skip entries with errors
                let _ = e;
            }
        }
    }

    // Sort by modification time, newest first
    files.sort_by_key(|b| std::cmp::Reverse(b.1));

    Ok(files.into_iter().map(|(p, _)| p).collect())
}

fn tool_list_directory(args: &Value, allowed_paths: &[PathBuf]) -> String {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let resolved = match resolve_and_validate(path, allowed_paths) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let entries = match std::fs::read_dir(&resolved) {
        Ok(entries) => entries,
        Err(e) => return format!("Error listing directory {}: {}", path, e),
    };

    let mut items: Vec<(String, String, u64)> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata().ok();
        let kind = metadata
            .as_ref()
            .map(|m| {
                if m.is_dir() {
                    "dir"
                } else if m.is_symlink() {
                    "link"
                } else {
                    "file"
                }
            })
            .unwrap_or("?");
        let size = metadata.map(|m| m.len()).unwrap_or(0);
        items.push((name, kind.to_string(), size));
    }

    items.sort_by(|a, b| a.0.cmp(&b.0));

    let mut result = String::new();
    for (name, kind, size) in &items {
        result.push_str(&format!("{:<6} {:>8}  {}\n", kind, size, name));
    }
    result.push_str(&format!("\n{} entries", items.len()));

    result
}

fn tool_memory(args: &Value, memory: &mut MemoryStore) -> String {
    let action = match args.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return "Error: missing required parameter 'action'".to_string(),
    };

    match action {
        "add" => {
            let content = match args.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "Error: 'add' requires 'content' parameter".to_string(),
            };
            match memory.add_memory(content.to_string()) {
                Ok(()) => "Memory entry saved.".to_string(),
                Err(e) => format!("Error saving memory: {}", e),
            }
        }
        "remove" => {
            let substring = match args.get("content").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => {
                    return "Error: 'remove' requires 'content' parameter (substring to match)"
                        .to_string();
                }
            };
            match memory.remove_memory(substring) {
                Ok(true) => "Memory entry removed.".to_string(),
                Ok(false) => "No matching memory entry found.".to_string(),
                Err(e) => format!("Error removing memory: {}", e),
            }
        }
        "replace" => {
            let old_text = match args.get("content").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => {
                    return "Error: 'replace' requires 'content' parameter (old text to find)"
                        .to_string();
                }
            };
            let new_text = match args.get("new_content").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return "Error: 'replace' requires 'new_content' parameter".to_string(),
            };
            match memory.replace_memory(old_text, new_text) {
                Ok(true) => "Memory entry updated.".to_string(),
                Ok(false) => "No matching memory entry found.".to_string(),
                Err(e) => format!("Error updating memory: {}", e),
            }
        }
        "list" => {
            let mut result = String::new();
            if memory.user_entries.is_empty() && memory.memory_entries.is_empty() {
                return "(no memory entries)".to_string();
            }
            if !memory.user_entries.is_empty() {
                result.push_str("── USER ──\n");
                for entry in &memory.user_entries {
                    result.push_str(&format!("  {}\n", entry));
                }
            }
            if !memory.memory_entries.is_empty() {
                result.push_str("── NOTES ──\n");
                for (i, entry) in memory.memory_entries.iter().enumerate() {
                    result.push_str(&format!("  [{}] {}\n", i + 1, entry));
                }
            }
            result
        }
        _ => format!(
            "Unknown memory action: '{}'. Use add, remove, replace, or list.",
            action
        ),
    }
}

fn tool_knowledge(args: &Value, kstore: &mut KnowledgeStore) -> String {
    use crate::kstore::Category;
    use std::str::FromStr;

    let action = match args.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return "Error: missing required parameter 'action'".to_string(),
    };

    match action {
        "get" => {
            let key = match args.get("key").and_then(|v| v.as_str()) {
                Some(k) => k,
                None => return "Error: 'get' requires 'key' parameter".to_string(),
            };
            match kstore.get(key) {
                Some(entry) => format!(
                    "{} = {} (category: {}, source: {}, updated: {})",
                    entry.key, entry.value, entry.category.as_str(),
                    match entry.source {
                        crate::kstore::Source::Observed => "observed",
                        crate::kstore::Source::Told => "told",
                        crate::kstore::Source::Inferred => "inferred",
                    },
                    entry.updated_at
                ),
                None => format!("No entry found for key '{}'.", key),
            }
        }
        "store" => {
            let key = match args.get("key").and_then(|v| v.as_str()) {
                Some(k) => k,
                None => return "Error: 'store' requires 'key' parameter".to_string(),
            };
            let value = match args.get("value").and_then(|v| v.as_str()) {
                Some(v) => v,
                None => return "Error: 'store' requires 'value' parameter".to_string(),
            };
            let category_str = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("fact");
            let category = match Category::from_str(category_str) {
                Ok(c) => c,
                Err(e) => return format!("Error: {}", e),
            };
            let source_str = args
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("told");
            let source = match crate::kstore::Source::from_str(source_str) {
                Ok(s) => s,
                Err(e) => return format!("Error: {}", e),
            };
            kstore.store(key, value, category, source);
            match kstore.save() {
                Ok(()) => format!("Stored: {} = {}", key, value),
                Err(e) => format!("Stored in memory but failed to persist: {}", e),
            }
        }
        "search" => {
            let query = match args.get("query").and_then(|v| v.as_str()) {
                Some(q) => q,
                None => return "Error: 'search' requires 'query' parameter".to_string(),
            };
            let results = kstore.search(query);
            if results.is_empty() {
                format!("No entries found matching prefix '{}'.", query)
            } else {
                let mut out = format!("Found {} entries:\n", results.len());
                for entry in &results {
                    out.push_str(&format!("  {} = {}\n", entry.key, entry.value));
                }
                out
            }
        }
        "list" => {
            if kstore.entries.is_empty() {
                return "(no knowledge entries)".to_string();
            }
            let groups = kstore.list_by_category();
            let mut out = String::new();
            for (category, entries) in &groups {
                out.push_str(&format!("[{}]\n", category));
                for entry in entries {
                    out.push_str(&format!("  {} = {}\n", entry.key, entry.value));
                }
            }
            out
        }
        "forget" => {
            let key = match args.get("key").and_then(|v| v.as_str()) {
                Some(k) => k,
                None => return "Error: 'forget' requires 'key' parameter".to_string(),
            };
            if kstore.forget(key) {
                match kstore.save() {
                    Ok(()) => format!("Forgot: {}", key),
                    Err(e) => format!("Removed from memory but failed to persist: {}", e),
                }
            } else {
                format!("No entry found for key '{}'.", key)
            }
        }
        _ => format!(
            "Unknown knowledge action: '{}'. Use get, store, search, list, or forget.",
            action
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Broad allow-list for tests: permits any path so file/exec tools run.
    fn allowed() -> Vec<PathBuf> {
        vec![PathBuf::from("/")]
    }

    #[test]
    fn tool_definitions_count() {
        assert_eq!(tool_definitions().len(), 8);
    }

    #[test]
    fn tools_json_format() {
        let tools = tools_json();
        assert_eq!(tools.len(), 8);
        for tool in &tools {
            assert_eq!(tool["type"], "function");
            assert!(tool["function"]["name"].is_string());
            assert!(tool["function"]["parameters"]["type"] == "object");
        }
    }

    #[test]
    fn dispatch_unknown_tool() {
        let mut mem = MemoryStore::default();
        let result = dispatch("nonexistent", &json!({}), &mut mem, &mut KnowledgeStore::default(), &allowed(), true);
        assert!(result.contains("Unknown tool"));
    }

    #[test]
    fn dispatch_missing_required_param() {
        let mut mem = MemoryStore::default();
        let result = dispatch("exec_command", &json!({}), &mut mem, &mut KnowledgeStore::default(), &allowed(), true);
        assert!(result.contains("missing required"));
    }

    #[test]
    fn write_and_read_file() {
        let mut mem = MemoryStore::default();
        let tmp = std::env::temp_dir().join("enchanter_test_write_read.txt");
        let path = tmp.to_string_lossy().to_string();

        let write_result = dispatch(
            "write_file",
            &json!({"path": path, "content": "hello world"}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(write_result.contains("Wrote"));

        let read_result = dispatch(
            "read_file",
            &json!({"path": path}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(read_result.contains("hello world"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn list_directory_works() {
        let mut mem = MemoryStore::default();
        let result = dispatch(
            "list_directory",
            &json!({"path": "/tmp"}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(!result.contains("Error"));
    }

    // exec_command's happy path re-execs the real enchanter binary as the
    // sandbox helper (see sandbox.rs), which a libtest binary can't stand in
    // for. End-to-end sandbox behavior (allow inside $HOME, deny outside) is
    // covered by tests/sandbox.rs against the compiled binary.

    #[test]
    fn edit_file_basic() {
        let mut mem = MemoryStore::default();
        let tmp = std::env::temp_dir().join("enchanter_test_edit.txt");
        let path = tmp.to_string_lossy().to_string();

        // Write initial content
        let _ = dispatch(
            "write_file",
            &json!({"path": &path, "content": "fn main() {\n    println!(\"hello\");\n}\n"}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );

        // Edit it
        let edit_result = dispatch(
            "edit_file",
            &json!({
                "path": &path,
                "old_string": "println!(\"hello\")",
                "new_string": "println!(\"world\")"
            }),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(edit_result.contains("Edited"));

        // Verify
        let read_result = dispatch(
            "read_file",
            &json!({"path": &path}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(read_result.contains("world"));
        assert!(!read_result.contains("hello"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn edit_file_requires_unique_match() {
        let mut mem = MemoryStore::default();
        let tmp = std::env::temp_dir().join("enchanter_test_edit_multi.txt");
        let path = tmp.to_string_lossy().to_string();

        // Write content with duplicates
        let _ = dispatch(
            "write_file",
            &json!({"path": &path, "content": "foo\nbar\nfoo\n"}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );

        let edit_result = dispatch(
            "edit_file",
            &json!({
                "path": &path,
                "old_string": "foo",
                "new_string": "baz"
            }),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(edit_result.contains("2 times") || edit_result.contains("unique"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn edit_file_replace_all() {
        let mut mem = MemoryStore::default();
        let tmp = std::env::temp_dir().join("enchanter_test_edit_all.txt");
        let path = tmp.to_string_lossy().to_string();

        let _ = dispatch(
            "write_file",
            &json!({"path": &path, "content": "foo\nbar\nfoo\n"}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );

        let edit_result = dispatch(
            "edit_file",
            &json!({
                "path": &path,
                "old_string": "foo",
                "new_string": "baz",
                "replace_all": true
            }),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(edit_result.contains("2 occurrence"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn search_files_by_name() {
        let mut mem = MemoryStore::default();
        let result = dispatch(
            "search_files",
            &json!({
                "pattern": "*.toml",
                "path": ".",
                "target": "files"
            }),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(result.contains("Cargo.toml"));
    }

    #[test]
    fn memory_add_and_list() {
        let mut mem = MemoryStore::default();
        let add_result = dispatch(
            "memory",
            &json!({
                "action": "add",
                "content": "project uses rust 1.85"
            }),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(add_result.contains("saved"));

        let list_result = dispatch(
            "memory",
            &json!({"action": "list"}),
            &mut mem,
            &mut KnowledgeStore::default(),
            &allowed(),
            true,
        );
        assert!(list_result.contains("rust 1.85"));
    }
}
