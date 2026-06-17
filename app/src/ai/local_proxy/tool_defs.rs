use serde_json::json;
use warp_multi_agent_api::ToolType;

use super::openai::{FunctionDefinition, ToolDefinition};

/// Build the list of OpenAI tool definitions for the given Warp supported tools.
pub fn tool_definitions_for(supported: &[i32]) -> Vec<ToolDefinition> {
    let supported_set: std::collections::HashSet<i32> = supported.iter().copied().collect();
    all_tool_definitions()
        .into_iter()
        .filter(|(t, _)| supported_set.contains(&(*t as i32)))
        .map(|(_, d)| d)
        .collect()
}

fn mk(name: &str, description: &str, parameters: serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        def_type: "function".into(),
        function: FunctionDefinition {
            name: name.into(),
            description: description.into(),
            parameters,
        },
    }
}

fn all_tool_definitions() -> Vec<(ToolType, ToolDefinition)> {
    vec![
        (
            ToolType::RunShellCommand,
            mk(
                "run_shell_command",
                "Execute a shell command in the user's terminal. \
                 Use for running programs, checking system state, installing packages, \
                 compiling code, running tests, and other shell operations.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        },
                        "is_read_only": {
                            "type": "boolean",
                            "description": "True if the command only reads state without modifying it"
                        },
                        "uses_pager": {
                            "type": "boolean",
                            "description": "True if the command uses a pager like 'less' or 'more'"
                        }
                    },
                    "required": ["command"]
                }),
            ),
        ),
        (
            ToolType::ReadFiles,
            mk(
                "read_files",
                "Read the contents of one or more files. \
                 Specify optional line ranges to read only part of a file.",
                json!({
                    "type": "object",
                    "properties": {
                        "files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": {
                                        "type": "string",
                                        "description": "Absolute or relative path to the file"
                                    }
                                },
                                "required": ["name"]
                            }
                        }
                    },
                    "required": ["files"]
                }),
            ),
        ),
        (
            ToolType::ApplyFileDiffs,
            mk(
                "apply_file_diffs",
                "Apply changes to files using search/replace diffs. \
                 Each diff specifies the exact content to find and the replacement. \
                 For new files use new_files; for deletions use deleted_files.",
                json!({
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "Brief description of what these changes accomplish"
                        },
                        "diffs": {
                            "type": "array",
                            "description": "List of search/replace diffs to apply",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_path": {
                                        "type": "string",
                                        "description": "Path to the file to modify"
                                    },
                                    "search": {
                                        "type": "string",
                                        "description": "Exact content to find and replace. Must match exactly."
                                    },
                                    "replace": {
                                        "type": "string",
                                        "description": "Content to insert in place of 'search'"
                                    }
                                },
                                "required": ["file_path", "search", "replace"]
                            }
                        },
                        "new_files": {
                            "type": "array",
                            "description": "New files to create with given content",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_path": { "type": "string" },
                                    "content": { "type": "string" }
                                },
                                "required": ["file_path", "content"]
                            }
                        },
                        "deleted_files": {
                            "type": "array",
                            "description": "Files to delete",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_path": { "type": "string" }
                                },
                                "required": ["file_path"]
                            }
                        }
                    },
                    "required": ["summary", "diffs"]
                }),
            ),
        ),
        (
            ToolType::Grep,
            mk(
                "grep",
                "Search for text patterns in files. \
                 Returns matching file paths and line numbers.",
                json!({
                    "type": "object",
                    "properties": {
                        "queries": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Search patterns or terms to look for"
                        },
                        "path": {
                            "type": "string",
                            "description": "Relative path to file or directory to search in. Defaults to current directory."
                        }
                    },
                    "required": ["queries"]
                }),
            ),
        ),
        (
            ToolType::FileGlobV2,
            mk(
                "file_glob_v2",
                "List files matching glob patterns. \
                 Supports ?, *, and [] wildcards. \
                 Use ** for recursive matching.",
                json!({
                    "type": "object",
                    "properties": {
                        "patterns": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Glob patterns to match file names"
                        },
                        "search_dir": {
                            "type": "string",
                            "description": "Directory to search in. Defaults to current directory."
                        },
                        "max_matches": {
                            "type": "integer",
                            "description": "Maximum number of matches to return. 0 means no limit.",
                            "default": 100
                        },
                        "max_depth": {
                            "type": "integer",
                            "description": "Maximum directory depth to search. 0 means no limit."
                        }
                    },
                    "required": ["patterns"]
                }),
            ),
        ),
        (
            ToolType::SearchCodebase,
            mk(
                "search_codebase",
                "Semantically search the codebase for relevant code snippets \
                 using natural language. Returns matching file contents.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural language description of what to find"
                        },
                        "path_filters": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional file path filters to narrow the search"
                        }
                    },
                    "required": ["query"]
                }),
            ),
        ),
    ]
}
