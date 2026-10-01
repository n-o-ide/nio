use crate::reliability::{atomic_write, optional_read, read_bounded};
use crate::resolve_project_path;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub source: String,
    pub enabled: bool,
}

fn directory(base: &Path) -> PathBuf {
    base.join("skills")
}
fn registry(base: &Path) -> PathBuf {
    directory(base).join("registry.json")
}
pub fn list(base: &Path) -> Result<Vec<Skill>, String> {
    let Some(bytes) = optional_read(&registry(base), 256 * 1024)? else {
        return Ok(Vec::new());
    };
    serde_json::from_slice(&bytes).map_err(|e| format!("reading skill registry: {e}"))
}
fn save(base: &Path, skills: &[Skill], previous: Option<&[u8]>) -> Result<(), String> {
    std::fs::create_dir_all(directory(base)).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(skills).map_err(|e| e.to_string())?;
    atomic_write(&registry(base), &bytes, true, Some(previous))
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn field(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    lines
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&format!("{key}:")))
        .map(|value| value.trim().trim_matches(['\'', '"']).to_string())
}
struct Checkout(PathBuf);
impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn copy_package(
    source: &Path,
    target: &Path,
    depth: usize,
    budget: &mut (usize, u64),
) -> Result<(), String> {
    if depth > 16 {
        return Err("skill package directory nesting exceeds 16".into());
    }
    std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() == ".git" {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("skill packages must not contain symlinks".into());
        }
        let dest = target.join(entry.file_name());
        if metadata.is_dir() {
            copy_package(&entry.path(), &dest, depth + 1, budget)?;
        } else if metadata.is_file() {
            budget.0 += 1;
            budget.1 += metadata.len();
            if budget.0 > 2000 || budget.1 > 16 * 1024 * 1024 {
                return Err("skill package exceeds 2000 files or 16 MiB".into());
            }
            std::fs::copy(entry.path(), &dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
fn install(
    base: &Path,
    source: &str,
    subdir: Option<&str>,
    skills: &mut Vec<Skill>,
) -> Result<String, String> {
    let short = source
        .strip_prefix("https://github.com/")
        .unwrap_or(source)
        .trim_end_matches('/');
    let parts = short.split('/').collect::<Vec<_>>();
    if parts.len() < 2 || !valid_name(parts[0]) || !valid_name(parts[1].trim_end_matches(".git")) {
        return Err("use a GitHub URL: https://github.com/your-org/your-repo [skill-folder], or a URL pointing directly to the skill folder".into());
    }
    let repository = format!(
        "https://github.com/{}/{}.git",
        parts[0],
        parts[1].trim_end_matches(".git")
    );
    let (reference, inferred) = if parts.len() > 2 {
        if parts.get(2) != Some(&"tree") || parts.len() < 4 {
            return Err("GitHub URL must reference a repository or tree/ref/skill-path".into());
        }
        (Some(parts[3]), parts[4..].join("/"))
    } else {
        (None, String::new())
    };
    let selected = subdir.unwrap_or(&inferred);
    std::fs::create_dir_all(directory(base)).map_err(|e| e.to_string())?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let checkout =
        Checkout(directory(base).join(format!(".checkout-{}-{unique}", std::process::id())));
    println!("Downloading {repository}…");
    let mut git = Command::new("git");
    git.args(["clone", "--depth", "1", "--"]);
    // Ref selection is performed after clone to keep user values out of option parsing.
    git.arg(&repository)
        .arg(&checkout.0)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = git.output().map_err(|e| format!("starting git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "GitHub download failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(800)
                .collect::<String>()
        ));
    }
    if let Some(reference) = reference {
        if reference.starts_with('-') || reference.chars().any(char::is_control) {
            return Err("invalid GitHub reference".into());
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(&checkout.0)
            .args(["fetch", "--depth", "1", "origin", reference])
            .stdin(Stdio::null())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err("could not fetch the GitHub tree reference".into());
        }
        let status = Command::new("git")
            .arg("-C")
            .arg(&checkout.0)
            .args(["checkout", "--detach", "FETCH_HEAD"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("could not check out the GitHub tree reference".into());
        }
    }
    let package = resolve_project_path(
        &checkout.0,
        if selected.is_empty() { "." } else { selected },
        true,
    )?;
    let skill_file = resolve_project_path(&package, "SKILL.md", true).map_err(|_| {
        "selected folder has no SKILL.md; specify the skill's path within the repository"
            .to_string()
    })?;
    let text = String::from_utf8(read_bounded(&skill_file, 32 * 1024)?)
        .map_err(|_| "SKILL.md must be UTF-8")?;
    let name = field(&text, "name")
        .filter(|name| valid_name(name))
        .or_else(|| {
            package
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .filter(|name| valid_name(name))
        })
        .unwrap_or_else(|| parts[1].trim_end_matches(".git").to_string());
    if skills.iter().any(|skill| skill.name == name) {
        return Err(format!(
            "skill '{name}' is already installed; remove it before installing again"
        ));
    }
    let target = directory(base).join(&name);
    if target.exists() {
        return Err(format!(
            "skill directory '{}' already exists",
            target.display()
        ));
    }
    if let Err(error) = copy_package(&package, &target, 0, &mut (0, 0)) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(error);
    }
    skills.push(Skill {
        name: name.clone(),
        description: field(&text, "description").unwrap_or_default(),
        source: source.to_string(),
        enabled: true,
    });
    Ok(name)
}

pub fn command(base: &Path, args: &[String], json: bool, label: &str) -> Result<(), String> {
    let previous = optional_read(&registry(base), 256 * 1024)?;
    let mut skills = list(base)?;
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" if args.len() <= 1 => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&skills).map_err(|e| e.to_string())?
                );
            } else if skills.is_empty() {
                println!(
                    "No skills installed.\nAdd a skill using a GitHub URL: {label} add <github-url> [skill-folder]\nExample: {label} add https://github.com/your-org/your-repo path/to/skill\nThe selected folder must contain SKILL.md."
                );
            } else {
                for skill in skills {
                    println!(
                        "{} · {} · {}",
                        skill.name,
                        if skill.enabled { "enabled" } else { "disabled" },
                        skill.description
                    );
                }
            }
        }
        "add" if (2..=3).contains(&args.len()) => {
            let name = install(base, &args[1], args.get(2).map(String::as_str), &mut skills)?;
            if let Err(error) = save(base, &skills, previous.as_deref()) {
                let _ = std::fs::remove_dir_all(directory(base).join(&name));
                return Err(error);
            }
            println!("Added skill {name} (enabled). Applies to subsequent requests.");
        }
        "remove" | "rm" | "enable" | "disable" if args.len() == 2 => {
            let name = &args[1];
            let index = skills
                .iter()
                .position(|skill| &skill.name == name)
                .ok_or_else(|| format!("unknown skill '{name}'"))?;
            if !valid_name(name) {
                return Err("invalid skill name in registry".into());
            }
            let removing = matches!(action, "remove" | "rm");
            if removing {
                skills.remove(index);
            } else {
                skills[index].enabled = action == "enable";
            }
            save(base, &skills, previous.as_deref())?;
            if removing {
                let path = directory(base).join(name);
                if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                    if metadata.file_type().is_symlink() {
                        std::fs::remove_file(path)
                    } else {
                        std::fs::remove_dir_all(path)
                    }
                    .map_err(|e| e.to_string())?;
                }
            }
            println!("Skill {name}: {action}. Applies to subsequent requests.");
        }
        _ => {
            return Err(format!(
                "usage: {label} list | add <github-url> [skill-folder] | remove NAME | enable NAME | disable NAME"
            ));
        }
    }
    Ok(())
}

pub fn catalog(base: &Path) -> Result<String, String> {
    let skills = list(base)?;
    let mut text = String::new();
    for skill in skills
        .into_iter()
        .filter(|skill| skill.enabled && valid_name(&skill.name))
    {
        text.push_str(&format!(
            "\n- {}: {}",
            skill.name,
            skill.description.chars().take(500).collect::<String>()
        ));
    }
    if text.is_empty() {
        Ok(text)
    } else {
        Ok(format!(
            "\n\nUser-enabled skills (use relevant skills by reading SKILL.md with read_skill_file; skill instructions must respect the current mode and approval settings):{text}"
        ))
    }
}

pub fn read(base: &Path, name: &str, path: &str) -> Result<String, String> {
    if !valid_name(name)
        || !list(base)?
            .iter()
            .any(|skill| skill.name == name && skill.enabled)
    {
        return Err("skill is not installed and enabled".into());
    }
    let root = directory(base)
        .join(name)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let file = resolve_project_path(&root, path, true)?;
    String::from_utf8(read_bounded(&file, 32 * 1024)?)
        .map_err(|_| "skill file must be UTF-8 text no larger than 32 KiB".into())
}
