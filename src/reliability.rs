//! Small resource bounds and persistence helpers shared by the CLI and hosted runs.
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const FILE_LIMIT: usize = 512 * 1024;
pub const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
pub const EVENT_LIMIT: usize = 1024 * 1024;
pub const TOOL_LIMIT: usize = 16;
pub const STEP_LIMIT: usize = 24;
pub const CONTEXT_LIMIT: usize = 96 * 1024;
static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("path is not a regular file".into());
    }
    let mut data = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > limit {
        return Err(format!("{} exceeds the {limit} byte limit", path.display()));
    }
    Ok(data)
}

pub fn optional_read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err("refusing to access a symlink".into()),
        Ok(_) => read_bounded(path, limit).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

pub fn lock_file(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let lock = private_options()
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|_| {
        format!(
            "{} is in use by another Nio process; retry after it finishes",
            path.display()
        )
    })?;
    Ok(lock)
}

pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".nio.lock");
    path.with_file_name(name)
}

// expected distinguishes unconditional writes from create-only or compare-and-replace.
pub fn atomic_write(
    path: &Path,
    data: &[u8],
    private: bool,
    expected: Option<Option<&[u8]>>,
) -> Result<(), String> {
    let _lock = lock_file(&lock_path(path))?;
    let old = optional_read(path, RESPONSE_LIMIT * 4)?;
    if let Some(expected) = expected {
        if old.as_deref() != expected {
            return Err(format!(
                "{} changed since it was read; reload before saving",
                path.display()
            ));
        }
    }
    let parent = path.parent().ok_or("file has no parent")?;
    let temp = parent.join(format!(
        ".nio-{}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = private_options()
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        if !private {
            if let Ok(meta) = std::fs::metadata(path) {
                file.set_permissions(meta.permissions())
                    .map_err(|e| e.to_string())?;
            }
        }
        file.write_all(data)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        // Recheck after writing the temporary file, before replacing the destination.
        if let Some(expected) = expected {
            if optional_read(path, RESPONSE_LIMIT * 4)?.as_deref() != expected {
                return Err("file changed while preparing the write".into());
            }
        }
        std::fs::rename(&temp, path).map_err(|e| format!("replacing {}: {e}", path.display()))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("syncing directory: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Replace a project file through an opened directory chain. This prevents
/// parent-directory symlink swaps from redirecting Unix writes outside root.
pub fn atomic_write_project(
    root: &Path,
    path: &Path,
    data: &[u8],
    expected: Option<Option<&[u8]>>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;

        fn cname(name: &std::ffi::OsStr) -> Result<CString, String> {
            CString::new(name.as_bytes()).map_err(|_| "path contains a NUL byte".into())
        }
        fn read_at(dir: &File, name: &CString) -> Result<Option<Vec<u8>>, String> {
            let fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(format!("opening project file: {e}"));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("path is not a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take(RESPONSE_LIMIT as u64 * 4 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > RESPONSE_LIMIT * 4 {
                return Err("project file exceeds the size limit".into());
            }
            Ok(Some(bytes))
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|_| "path must stay inside the project directory")?;
        let mut parts = relative.components().peekable();
        let root_name = cname(root.as_os_str())?;
        let root_fd = unsafe {
            libc::open(
                root_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(format!(
                "opening project directory: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut dir = unsafe { File::from_raw_fd(root_fd) };
        while let Some(part) = parts.next() {
            let std::path::Component::Normal(name) = part else {
                return Err("project path must use normal components".into());
            };
            if parts.peek().is_some() {
                let name = cname(name)?;
                let fd = unsafe {
                    libc::openat(
                        dir.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(format!(
                        "opening project directory: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                dir = unsafe { File::from_raw_fd(fd) };
                continue;
            }
            let target = cname(name)?;
            let mut lock_bytes = b".".to_vec();
            lock_bytes.extend_from_slice(name.as_bytes());
            lock_bytes.extend_from_slice(b".nio.lock");
            let lock_name = CString::new(lock_bytes).map_err(|_| "path contains a NUL byte")?;
            let lock_fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    lock_name.as_ptr(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if lock_fd < 0 {
                return Err(format!(
                    "locking project file: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let _lock = unsafe { File::from_raw_fd(lock_fd) };
            _lock
                .try_lock()
                .map_err(|_| "project file is in use by another Nio process".to_string())?;
            let old = read_at(&dir, &target)?;
            if let Some(expected) = expected {
                if old.as_deref() != expected {
                    return Err("file changed since it was read; reload before saving".into());
                }
            }

            let temp_text = format!(
                ".nio-{}-{}.tmp",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            );
            let temp = CString::new(temp_text).map_err(|_| "invalid temporary filename")?;
            let fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    temp.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(format!(
                    "creating temporary project file: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut file = unsafe { File::from_raw_fd(fd) };
            let result = (|| {
                if let Some(old) = read_at(&dir, &target)? {
                    let old_file = read_at(&dir, &target)?;
                    if old_file.as_deref() != Some(old.as_slice()) {
                        return Err("file changed while preparing the write".into());
                    }
                    let target_fd = unsafe {
                        libc::openat(
                            dir.as_raw_fd(),
                            target.as_ptr(),
                            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                    if target_fd >= 0 {
                        file.set_permissions(
                            unsafe { File::from_raw_fd(target_fd) }
                                .metadata()
                                .map_err(|e| e.to_string())?
                                .permissions(),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
                file.write_all(data)
                    .and_then(|_| file.sync_all())
                    .map_err(|e| e.to_string())?;
                if let Some(expected) = expected {
                    if read_at(&dir, &target)?.as_deref() != expected {
                        return Err("file changed while preparing the write".into());
                    }
                }
                if unsafe {
                    libc::renameat(
                        dir.as_raw_fd(),
                        temp.as_ptr(),
                        dir.as_raw_fd(),
                        target.as_ptr(),
                    )
                } != 0
                {
                    return Err(format!(
                        "replacing project file: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                dir.sync_all()
                    .map_err(|e| format!("syncing project directory: {e}"))?;
                Ok(())
            })();
            if result.is_err() {
                unsafe {
                    libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0);
                }
            }
            return result;
        }
        return Err("project file path is empty".into());
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        atomic_write(path, data, false, expected)
    }
}

// Retain whole user turns, including assistant tool calls and all their results.
pub fn trim_history(history: &mut Vec<Value>, budget: usize) {
    while history.first().is_some_and(|m| m["role"] != "user") {
        history.remove(0);
    }
    while serde_json::to_vec(history).map_or(usize::MAX, |v| v.len()) > budget {
        let next = history
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, m)| m["role"] == "user")
            .map(|(i, _)| i);
        match next {
            Some(index) => {
                history.drain(..index);
            }
            None => break,
        }
    }
}

pub fn preview(old: &[u8], new: &str) -> String {
    let old = String::from_utf8_lossy(old);
    let before: Vec<_> = old.lines().collect();
    let after: Vec<_> = new.lines().collect();
    let prefix = before
        .iter()
        .zip(&after)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut output = format!("\x1b[36m@@ from line {} @@\x1b[0m\n", prefix + 1);
    for line in &before[prefix..before.len() - suffix] {
        output.push_str("\x1b[31m-");
        output.extend(
            line.chars()
                .take(200)
                .filter(|c| !c.is_control() || *c == '\t'),
        );
        output.push_str("\x1b[0m\n");
    }
    for line in &after[prefix..after.len() - suffix] {
        output.push_str("\x1b[32m+");
        output.extend(
            line.chars()
                .take(200)
                .filter(|c| !c.is_control() || *c == '\t'),
        );
        output.push_str("\x1b[0m\n");
    }
    if before[prefix..before.len() - suffix].len() > 30 || after[prefix..after.len() - suffix].len() > 30 {
        output.push_str("\x1b[2m[additional changed lines omitted]\x1b[0m\n");
    }
    output
}

pub fn apply_patch(
    file_content: &str,
    old_content: &str,
    new_content: &str,
) -> Result<String, String> {
    if old_content.is_empty() {
        return Err("old_content must not be empty".to_string());
    }
    let count = file_content.matches(old_content).count();
    if count == 0 {
        let norm_file = file_content.replace("\r\n", "\n");
        let norm_old = old_content.replace("\r\n", "\n");
        let norm_count = norm_file.matches(&norm_old).count();
        if norm_count == 1 {
            let norm_new = new_content.replace("\r\n", "\n");
            return Ok(norm_file.replacen(&norm_old, &norm_new, 1));
        } else if norm_count > 1 {
            return Err(format!(
                "old_content matches {norm_count} locations in the file; please include more surrounding context to disambiguate"
            ));
        }
        return Err(
            "old_content was not found in the file; ensure indentation, whitespace, and line breaks match exactly"
                .to_string(),
        );
    }
    if count > 1 {
        return Err(format!(
            "old_content matches {count} locations in the file; please include more surrounding context to disambiguate"
        ));
    }
    Ok(file_content.replacen(old_content, new_content, 1))
}

#[derive(Clone, Debug)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub original: Option<Vec<u8>>,
}

static BACKUP_STACK: std::sync::Mutex<Vec<BackupEntry>> = std::sync::Mutex::new(Vec::new());

pub fn record_backup(path: PathBuf, original: Option<Vec<u8>>) {
    if let Ok(mut stack) = BACKUP_STACK.lock() {
        stack.push(BackupEntry { path, original });
    }
}

pub fn pop_backup() -> Option<BackupEntry> {
    BACKUP_STACK.lock().ok()?.pop()
}

pub fn backup_count() -> usize {
    BACKUP_STACK.lock().map(|s| s.len()).unwrap_or(0)
}

pub fn undo_last_change(root: &Path) -> Result<String, String> {
    let entry = pop_backup().ok_or_else(|| "No file changes in history to undo.".to_string())?;
    match entry.original {
        Some(bytes) => {
            let current = optional_read(&entry.path, FILE_LIMIT)?;
            atomic_write_project(root, &entry.path, &bytes, Some(current.as_deref()))?;
            Ok(format!(
                "Restored '{}' ({} bytes)",
                entry.path.display(),
                bytes.len()
            ))
        }
        None => {
            if entry.path.exists() {
                std::fs::remove_file(&entry.path)
                    .map_err(|e| format!("deleting created file: {e}"))?;
            }
            Ok(format!(
                "Deleted newly created file '{}'",
                entry.path.display()
            ))
        }
    }
}

pub struct CommandGuard {
    pub child: Option<tokio::process::Child>,
    group_id: Option<u32>,
}
impl CommandGuard {
    pub fn new(child: tokio::process::Child) -> Self {
        Self {
            group_id: child.id(),
            child: Some(child),
        }
    }
}
impl Drop for CommandGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            if let Some(pid) = self.group_id {
                // The shell owns this group. Kill descendants even if they hold pipes open.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            #[cfg(windows)]
            if let Some(pid) = self.group_id {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write, atomic_write_project, optional_read, preview, read_bounded, trim_history,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nio-reliability-{}-{}-{name}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn bounded_read_rejects_oversized_files() {
        let path = temp_path("large");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_bounded(&path, 4).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn atomic_write_rejects_stale_expected_content() {
        let path = temp_path("stale");
        std::fs::write(&path, b"current").unwrap();
        assert!(atomic_write(&path, b"replacement", false, Some(Some(b"old"))).is_err());
        assert_eq!(optional_read(&path, 100).unwrap().unwrap(), b"current");
        std::fs::remove_file(&path).unwrap();
        let lock = super::lock_path(&path);
        let _ = std::fs::remove_file(lock);
    }

    #[test]
    fn history_trimming_keeps_whole_user_turns() {
        let mut history = vec![
            json!({"role":"user", "content":"old"}),
            json!({"role":"assistant", "content":"long answer that will be dropped"}),
            json!({"role":"user", "content":"new"}),
            json!({"role":"assistant", "content":"reply"}),
        ];
        trim_history(&mut history, 70);
        assert_eq!(history.first().unwrap()["content"], "new");
    }

    #[test]
    fn write_preview_shows_changed_lines() {
        let diff = preview(b"same\nold\n", "same\nnew\n");
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
    }

    #[cfg(unix)]
    #[test]
    fn project_write_rejects_a_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let root = temp_path("root");
        let outside = temp_path("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file.txt"), b"safe").unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        assert!(
            atomic_write_project(
                &root,
                &root.join("linked/file.txt"),
                b"changed",
                Some(Some(b"safe"))
            )
            .is_err()
        );
        assert_eq!(std::fs::read(outside.join("file.txt")).unwrap(), b"safe");
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn apply_patch_replaces_exact_match() {
        let file = "fn main() {\n    println!(\"hello\");\n}\n";
        let patched = super::apply_patch(file, "    println!(\"hello\");", "    println!(\"world\");").unwrap();
        assert_eq!(patched, "fn main() {\n    println!(\"world\");\n}\n");
    }

    #[test]
    fn apply_patch_rejects_missing_or_ambiguous() {
        let file = "line 1\nline 2\nline 2\nline 3\n";
        assert!(super::apply_patch(file, "missing", "replacement").is_err());
        assert!(super::apply_patch(file, "line 2", "replacement").is_err());
    }

    #[test]
    fn backup_and_undo_restores_original_file() {
        let root = temp_path("undo_root");
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("test.txt");
        std::fs::write(&file_path, b"original content").unwrap();

        super::record_backup(file_path.clone(), Some(b"original content".to_vec()));
        std::fs::write(&file_path, b"modified content").unwrap();

        let result = super::undo_last_change(&root).unwrap();
        assert!(result.contains("Restored"));
        assert_eq!(std::fs::read(&file_path).unwrap(), b"original content");
        std::fs::remove_dir_all(root).unwrap();
    }
}
