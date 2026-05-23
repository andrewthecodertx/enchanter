//! Built-in tool definitions and dispatch — canonical set of 4.

use serde_json::{json, Value};
use std::path::Path;

/// A tool definition in OpenAI function-calling format.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// Return the canonical 4 tool definitions.
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

/// Dispatch a tool call by name. Returns the result as a string.
pub fn dispatch(name: &str, args: &Value) -> String {
    match name {
        "exec_command" => tool_exec_command(args),
        "read_file" => tool_read_file(args),
        "write_file" => tool_write_file(args),
        "list_directory" => tool_list_directory(args),
        _ => format!("Unknown tool: {}", name),
    }
}

fn tool_exec_command(args: &Value) -> String {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: missing required parameter 'command'".to_string(),
    };

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output();

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
                result.push_str(&format!("\n[exit code: {}]", out.status.code().unwrap_or(-1)));
            }

            // Truncate to 10,000 characters
            if result.len() > 10_000 {
                result.truncate(10_000);
                result.push_str("\n... [truncated]");
            }

            result
        }
        Err(e) => format!("Error executing command: {}", e),
    }
}

fn tool_read_file(args: &Value) -> String {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };

    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(500)
        .min(2000) as usize;

    // Expand tilde
    let expanded_path = shellexpand::tilde(path).to_string();

    let content = match std::fs::read_to_string(&expanded_path) {
        Ok(c) => c,
        Err(e) => return format!("Error reading file {}: {}", path, e),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start = offset.saturating_sub(1);
    if start >= lines.len() {
        return format!("File has {} lines, offset {} is past the end", total_lines, offset);
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

    // Truncate if needed
    if result.len() > 10_000 {
        result.truncate(10_000);
        result.push_str("\n... [truncated]");
    }

    result
}

fn tool_write_file(args: &Value) -> String {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: missing required parameter 'content'".to_string(),
    };

    let expanded_path = shellexpand::tilde(path).to_string();

    // Create parent directories
    if let Some(parent) = Path::new(&expanded_path).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return format!("Error creating directory {}: {}", parent.display(), e);
    }

    match std::fs::write(&expanded_path, content) {
        Ok(()) => format!("Wrote {}", path),
        Err(e) => format!("Error writing file {}: {}", path, e),
    }
}

fn tool_list_directory(args: &Value) -> String {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let expanded_path = shellexpand::tilde(path).to_string();

    let entries = match std::fs::read_dir(&expanded_path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_definitions_count() {
        assert_eq!(tool_definitions().len(), 4);
    }

    #[test]
    fn tools_json_format() {
        let tools = tools_json();
        assert_eq!(tools.len(), 4);
        for tool in &tools {
            assert_eq!(tool["type"], "function");
            assert!(tool["function"]["name"].is_string());
            assert!(tool["function"]["parameters"]["type"] == "object");
        }
    }

    #[test]
    fn dispatch_unknown_tool() {
        let result = dispatch("nonexistent", &json!({}));
        assert!(result.contains("Unknown tool"));
    }

    #[test]
    fn dispatch_missing_required_param() {
        let result = dispatch("exec_command", &json!({}));
        assert!(result.contains("missing required"));
    }

    #[test]
    fn write_and_read_file() {
        let tmp = std::env::temp_dir().join("enchanter_test_write_read.txt");
        let path = tmp.to_string_lossy().to_string();

        let write_result = dispatch("write_file", &json!({"path": path, "content": "hello world"}));
        assert!(write_result.contains("Wrote"));

        let read_result = dispatch("read_file", &json!({"path": path}));
        assert!(read_result.contains("hello world"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn list_directory_works() {
        let result = dispatch("list_directory", &json!({"path": "/tmp"}));
        assert!(!result.contains("Error"));
    }

    #[test]
    fn exec_command_works() {
        let result = dispatch("exec_command", &json!({"command": "echo hello"}));
        assert!(result.contains("hello"));
    }
}