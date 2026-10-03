//! Custom snippets & reusable functions subsystem for NioAI.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub runner: Option<String>,
    pub path: PathBuf,
    pub is_project: bool,
}

pub fn project_snippets_dir(root: &Path) -> PathBuf {
    root.join(".nio").join("snippets")
}

pub fn global_snippets_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("cannot determine user home directory")?;
    let path = PathBuf::from(home).join(".nio").join("snippets");
    Ok(path)
}

fn parse_snippet_header(path: &Path, is_project: bool) -> Option<Snippet> {
    let content = std::fs::read_to_string(path).ok()?;
    let file_stem = path.file_stem()?.to_string_lossy().to_string();

    let mut name = file_stem;
    let mut description = String::new();
    let mut tags = Vec::new();
    let mut runner = None;

    for line in content.lines().take(20) {
        let trimmed = line.trim();
        // Look for comment lines starting with #, //, or --
        let comment = if let Some(rest) = trimmed.strip_prefix("#") {
            rest.trim()
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            rest.trim()
        } else if let Some(rest) = trimmed.strip_prefix("--") {
            rest.trim()
        } else {
            continue;
        };

        if let Some(val) = comment.strip_prefix("snippet:") {
            let val = val.trim();
            if !val.is_empty() {
                name = val.to_string();
            }
        } else if let Some(val) = comment.strip_prefix("description:") {
            description = val.trim().to_string();
        } else if let Some(val) = comment.strip_prefix("tags:") {
            tags = val
                .split(',')
                .map(|t| t.trim().to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect();
        } else if let Some(val) = comment.strip_prefix("runner:") {
            runner = Some(val.trim().to_string());
        }
    }

    if description.is_empty() {
        description = format!("Snippet: {name}");
    }

    Some(Snippet {
        name,
        description,
        tags,
        runner,
        path: path.to_path_buf(),
        is_project,
    })
}

pub fn list(root: &Path) -> Vec<Snippet> {
    let mut snippets = Vec::new();

    // 1. Project snippets (.nio/snippets/)
    let project_dir = project_snippets_dir(root);
    if project_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(project_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(snippet) = parse_snippet_header(&path, true) {
                        snippets.push(snippet);
                    }
                }
            }
        }
    }

    // 2. Global snippets (~/.nio/snippets/)
    if let Ok(global_dir) = global_snippets_dir() {
        if global_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(global_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        let name = path.file_stem().unwrap_or_default().to_string_lossy();
                        // Avoid duplicates if project snippet overrides global
                        if !snippets.iter().any(|s| s.name == name) {
                            if let Some(snippet) = parse_snippet_header(&path, false) {
                                snippets.push(snippet);
                            }
                        }
                    }
                }
            }
        }
    }

    snippets.sort_by(|a, b| a.name.cmp(&b.name));
    snippets
}

pub fn search(root: &Path, query: &str) -> Vec<Snippet> {
    let q = query.to_ascii_lowercase();
    list(root)
        .into_iter()
        .filter(|s| {
            s.name.to_ascii_lowercase().contains(&q)
                || s.description.to_ascii_lowercase().contains(&q)
                || s.tags.iter().any(|t| t.contains(&q))
        })
        .collect()
}

pub fn read(root: &Path, name: &str) -> Result<String, String> {
    let snippets = list(root);
    let snippet = snippets
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("snippet '{name}' not found"))?;

    std::fs::read_to_string(&snippet.path)
        .map_err(|e| format!("reading snippet '{}': {e}", snippet.path.display()))
}

pub fn run(root: &Path, name: &str, args: &[String]) -> Result<String, String> {
    let snippets = list(root);
    let snippet = snippets
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("snippet '{name}' not found"))?;

    let runner = if let Some(r) = &snippet.runner {
        r.clone()
    } else {
        match snippet
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
        {
            "py" => "python3".to_string(),
            "sh" => "bash".to_string(),
            "js" => "node".to_string(),
            "ts" => "npx ts-node".to_string(),
            "rb" => "ruby".to_string(),
            _ => "bash".to_string(),
        }
    };

    let mut parts = runner.split_whitespace();
    let program = parts.next().ok_or("invalid runner command")?;
    let runner_args = parts.collect::<Vec<_>>();

    let mut cmd = Command::new(program);
    for arg in runner_args {
        cmd.arg(arg);
    }
    cmd.arg(&snippet.path);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.current_dir(root);

    let output = cmd
        .output()
        .map_err(|e| format!("executing snippet with runner '{program}': {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "Snippet failed (exit code {}):\n{}{}",
            output.status.code().unwrap_or(-1),
            stdout,
            stderr
        ));
    }

    Ok(format!("{}{}", stdout, stderr))
}

pub fn add(root: &Path, source: &Path, global: bool) -> Result<String, String> {
    if !source.is_file() {
        return Err(format!(
            "source snippet file '{}' does not exist",
            source.display()
        ));
    }
    let filename = source.file_name().ok_or("source file has no name")?;

    let target_dir = if global {
        let dir = global_snippets_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        dir
    } else {
        let dir = project_snippets_dir(root);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        dir
    };

    let dest = target_dir.join(filename);
    std::fs::copy(source, &dest).map_err(|e| format!("copying snippet: {e}"))?;

    Ok(format!(
        "Added snippet to {} ({})",
        dest.display(),
        if global { "global" } else { "project" }
    ))
}

pub fn command(root: &Path, args: &[String]) -> Result<(), String> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" => {
            let snippets = list(root);
            if snippets.is_empty() {
                println!("No snippets found.");
                println!("Add snippets to .nio/snippets/ (project) or ~/.nio/snippets/ (global).");
                return Ok(());
            }
            println!("Installed Snippets ({} total):", snippets.len());
            for s in snippets {
                let tags = if s.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", s.tags.join(", "))
                };
                let scope = if s.is_project { "project" } else { "global" };
                println!("  • {:<16} ({scope}){tags} - {}", s.name, s.description);
            }
        }
        "search" => {
            let query = args.get(1).map(String::as_str).unwrap_or("");
            let snippets = search(root, query);
            if snippets.is_empty() {
                println!("No snippets found matching '{query}'.");
                return Ok(());
            }
            println!("Found {} snippet(s) matching '{query}':", snippets.len());
            for s in snippets {
                let scope = if s.is_project { "project" } else { "global" };
                println!("  • {:<16} ({scope}) - {}", s.name, s.description);
            }
        }
        "read" => {
            let name = args.get(1).ok_or("usage: nio snippets read <name>")?;
            println!("{}", read(root, name)?);
        }
        "run" => {
            let name = args
                .get(1)
                .ok_or("usage: nio snippets run <name> [args...]")?;
            let snippet_args = args.iter().skip(2).cloned().collect::<Vec<_>>();
            let output = run(root, name, &snippet_args)?;
            print!("{output}");
        }
        "add" => {
            let source_path = args
                .get(1)
                .ok_or("usage: nio snippets add <file> [--global]")?;
            let is_global = args.iter().any(|a| a == "--global" || a == "-g");
            println!("{}", add(root, Path::new(source_path), is_global)?);
        }
        _ => {
            println!("Usage:");
            println!("  nio snippets [list]                  List available snippets");
            println!("  nio snippets search <query>          Search snippets by tag or name");
            println!("  nio snippets read <name>             Display snippet code");
            println!("  nio snippets run <name> [args...]    Execute a snippet");
            println!("  nio snippets add <file> [--global]   Register a new snippet");
        }
    }
    Ok(())
}
