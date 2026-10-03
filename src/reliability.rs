//! Small resource bounds and persistence helpers shared by the CLI and hosted runs.
use crossterm::terminal;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const FILE_LIMIT: usize = 512 * 1024;
pub const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
pub const EVENT_LIMIT: usize = 1024 * 1024;
pub const TOOL_LIMIT: usize = 16;
pub const STEP_LIMIT: usize = 128;
// A request is measured as serialized JSON bytes, which is substantially
// smaller than the token limit advertised by current hosted models. Keep a
// generous local ceiling while leaving room for the provider's output budget.
pub const CONTEXT_LIMIT: usize = 512 * 1024;
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
    for _ in 0..20 {
        if lock.try_lock().is_ok() {
            return Ok(lock);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
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
    replace_project_file(root, path, Some(data), expected)
}

fn replace_project_file(
    root: &Path,
    path: &Path,
    data: Option<&[u8]>,
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
            let mut locked = false;
            for _ in 0..20 {
                if _lock.try_lock().is_ok() {
                    locked = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if !locked {
                _lock
                    .try_lock()
                    .map_err(|_| "project file is in use by another Nio process".to_string())?;
            }
            let old = read_at(&dir, &target)?;
            if let Some(expected) = expected {
                if old.as_deref() != expected {
                    return Err("file changed since it was read; reload before saving".into());
                }
            }

            let Some(data) = data else {
                if old.is_some()
                    && unsafe { libc::unlinkat(dir.as_raw_fd(), target.as_ptr(), 0) } != 0
                {
                    return Err(format!(
                        "deleting project file: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                return dir
                    .sync_all()
                    .map_err(|e| format!("syncing project directory: {e}"));
            };

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
        let input = path.to_str().ok_or("path is not valid UTF-8")?;
        let checked = super::resolve_project_path(root, input, false)?;
        if checked != path {
            return Err("project path changed".into());
        }
        match data {
            Some(data) => atomic_write(path, data, false, expected),
            None => {
                let _lock = lock_file(&lock_path(path))?;
                let current = optional_read(path, FILE_LIMIT)?;
                if expected.is_some_and(|expected| current.as_deref() != expected) {
                    return Err("file changed since it was read".into());
                }
                if current.is_some() {
                    std::fs::remove_file(path).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
        }
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

pub fn preview_for_path(path: &str, old: &[u8], new: &str) -> String {
    preview_for_path_at(path, old, new, 1)
}

pub fn preview_summary(path: &str, old: &[u8], new: &str) -> String {
    let old = String::from_utf8_lossy(old);
    let before: Vec<_> = old.lines().collect();
    let after: Vec<_> = new.lines().collect();
    let (prefix, old_end, new_end) = changed_line_ranges(&before, &after);
    format!(
        "Edited {path} (+{} -{})",
        new_end.saturating_sub(prefix),
        old_end.saturating_sub(prefix)
    )
}

/// A plain unified diff, with matching lines retained instead of treating
/// everything between the first and last changed lines as a replacement.
pub fn edit_report(path: &str, old: &[u8], new: &str) -> String {
    if old == new.as_bytes() {
        return format!("No changes to {path}; content already matches.");
    }
    let old = String::from_utf8_lossy(old);
    let before: Vec<_> = old.lines().collect();
    let after: Vec<_> = new.lines().collect();
    let (prefix, old_end, new_end) = changed_line_ranges(&before, &after);
    let left = &before[prefix..old_end];
    let right = &after[prefix..new_end];
    let mut operations = before[..prefix]
        .iter()
        .map(|line| (' ', *line))
        .collect::<Vec<_>>();
    let columns = right.len() + 1;
    // Bound the matrix for very large replacements; common single-block edits
    // have a small middle after trimming the unchanged prefix and suffix.
    if (left.len() + 1).saturating_mul(columns) <= 1_000_000 {
        let mut lengths = vec![0u32; (left.len() + 1) * columns];
        for i in (0..left.len()).rev() {
            for j in (0..right.len()).rev() {
                lengths[i * columns + j] = if left[i] == right[j] {
                    lengths[(i + 1) * columns + j + 1] + 1
                } else {
                    lengths[(i + 1) * columns + j].max(lengths[i * columns + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < left.len() || j < right.len() {
            if i < left.len() && j < right.len() && left[i] == right[j] {
                operations.push((' ', left[i]));
                i += 1;
                j += 1;
            } else if i < left.len()
                && (j == right.len()
                    || lengths[(i + 1) * columns + j] >= lengths[i * columns + j + 1])
            {
                operations.push(('-', left[i]));
                i += 1;
            } else {
                operations.push(('+', right[j]));
                j += 1;
            }
        }
    } else {
        operations.extend(left.iter().map(|line| ('-', *line)));
        operations.extend(right.iter().map(|line| ('+', *line)));
    }
    operations.extend(before[old_end..].iter().map(|line| (' ', *line)));
    let additions = operations.iter().filter(|(kind, _)| *kind == '+').count();
    let removals = operations.iter().filter(|(kind, _)| *kind == '-').count();
    if additions == 0 && removals == 0 {
        return format!("Updated {path} (line endings or final newline changed).");
    }
    let mut result = format!(
        "Edited {path} (+{additions} -{removals})\ndiff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n"
    );
    let mut ranges = Vec::<(usize, usize)>::new();
    for (index, (kind, _)) in operations.iter().enumerate() {
        if *kind == ' ' {
            continue;
        }
        let start = index.saturating_sub(3);
        let end = (index + 4).min(operations.len());
        if let Some(last) = ranges.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            ranges.push((start, end));
        }
    }
    let mut positions = vec![(1usize, 1usize)];
    for (kind, _) in &operations {
        let (old, new) = *positions.last().unwrap();
        positions.push((
            old + usize::from(*kind != '+'),
            new + usize::from(*kind != '-'),
        ));
    }
    for (start, end) in ranges {
        let (old_start, new_start) = positions[start];
        let old_count = positions[end].0 - old_start;
        let new_count = positions[end].1 - new_start;
        result.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            if old_count == 0 {
                old_start - 1
            } else {
                old_start
            },
            old_count,
            if new_count == 0 {
                new_start - 1
            } else {
                new_start
            },
            new_count
        ));
        for (kind, line) in &operations[start..end] {
            result.push(*kind);
            result.extend(
                line.chars()
                    .filter(|character| !character.is_control() || *character == '\t'),
            );
            result.push('\n');
        }
    }
    result
}

pub fn preview_replacement(
    path: &str,
    original: &str,
    old_fragment: &str,
    new_fragment: &str,
) -> String {
    let Some(byte_offset) = original.find(old_fragment) else {
        return preview_for_path(
            path,
            original.as_bytes(),
            &original.replace(old_fragment, new_fragment),
        );
    };
    let start_line = original[..byte_offset].lines().count() + 1;
    preview_for_path_at(path, old_fragment.as_bytes(), new_fragment, start_line)
}

fn preview_for_path_at(path: &str, old: &[u8], new: &str, start_line: usize) -> String {
    let old = String::from_utf8_lossy(old);
    let before: Vec<_> = old.lines().collect();
    let after: Vec<_> = new.lines().collect();
    let (prefix, old_change_end, new_change_end) = changed_line_ranges(&before, &after);
    let context_start = prefix.saturating_sub(3);
    let old_context_end = (old_change_end + 3).min(before.len());
    let new_context_end = (new_change_end + 3).min(after.len());
    let mut output = format!(
        "\x1b[1;38;5;244mdiff --git a/{path} b/{path}\x1b[0m\n\x1b[38;5;244m--- a/{path}\n+++ b/{path}\x1b[0m\n"
    );
    output.push_str(&format!(
        "\x1b[1;38;5;39m@@ -{},{} +{},{} @@\x1b[0m\n",
        start_line + context_start,
        old_context_end - context_start,
        start_line + context_start,
        new_context_end - context_start
    ));
    let width = terminal::size()
        .map(|(width, _)| width as usize)
        .unwrap_or(120)
        .saturating_sub(4)
        .clamp(20, 200);
    for line in &before[context_start..prefix] {
        append_diff_line(&mut output, ' ', line, 244, width);
    }
    let mut omitted = false;
    for line in &before[prefix..old_change_end] {
        if output.lines().count() > 33 {
            omitted = true;
            break;
        }
        append_diff_line(&mut output, '-', line, 203, width);
    }
    for line in &after[prefix..new_change_end] {
        if output.lines().count() > 63 {
            omitted = true;
            break;
        }
        append_diff_line(&mut output, '+', line, 114, width);
    }
    for line in &after[new_change_end..new_context_end] {
        append_diff_line(&mut output, ' ', line, 244, width);
    }
    if omitted {
        output.push_str("\x1b[2m  … remaining changed lines omitted …\x1b[0m\n");
    }
    let removals = old_change_end - prefix;
    let additions = new_change_end - prefix;
    output.push_str(&format!(
        "\x1b[2m  {additions} additions, {removals} removals\x1b[0m\n"
    ));
    output
}

fn changed_line_ranges(before: &[&str], after: &[&str]) -> (usize, usize, usize) {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(old_line, new_line)| old_line == new_line)
        .count();
    let max_suffix = before.len().min(after.len()).saturating_sub(prefix);
    let suffix = (0..max_suffix)
        .take_while(|offset| before[before.len() - offset - 1] == after[after.len() - offset - 1])
        .count();
    (prefix, before.len() - suffix, after.len() - suffix)
}

fn append_diff_line(output: &mut String, marker: char, line: &str, color: u8, width: usize) {
    output.push_str(&format!("\x1b[38;5;{color}m{marker}",));
    let safe_line: String = line
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .take(width.saturating_sub(1))
        .collect();
    output.push_str(&safe_line);
    output.push_str("\x1b[0m\n");
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
        let norm_new = new_content.replace("\r\n", "\n");
        if !norm_new.trim().is_empty() && norm_file.matches(&norm_new).count() == 1 {
            // A repeated patch may target a block that an earlier call already
            // replaced. Preserve the original bytes, including line endings.
            return Ok(file_content.to_string());
        }
        return Err(
            "old_content was not found in the current file; read_file again and retry using the latest exact content, including whitespace and line breaks"
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BackupEntry {
    path: PathBuf,
    original: Option<Vec<u8>>,
    written: Vec<u8>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct UndoJournal {
    version: u32,
    root: PathBuf,
    entries: Vec<BackupEntry>,
}

const UNDO_LIMIT: usize = 8 * 1024 * 1024;

fn undo_path(root: &Path) -> Result<PathBuf, String> {
    // Stable FNV-1a identifies the path across Rust/toolchain upgrades. The
    // stored root is checked separately, so collisions cannot mix projects.
    let encoded = serde_json::to_vec(root).map_err(|e| e.to_string())?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in encoded {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    let config = super::config_path()?;
    Ok(config
        .parent()
        .ok_or("config has no parent")?
        .join("undo")
        .join(format!("{hash:016x}.json")))
}

fn load_undo(path: &Path, root: &Path) -> Result<UndoJournal, String> {
    let Some(bytes) = optional_read(path, UNDO_LIMIT)? else {
        return Ok(UndoJournal {
            version: 1,
            root: root.to_path_buf(),
            entries: Vec::new(),
        });
    };
    let journal: UndoJournal =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid undo journal: {e}"))?;
    if journal.version != 1 || journal.root != root {
        return Err("undo journal belongs to a different project or unsupported version".into());
    }
    Ok(journal)
}

fn save_undo(path: &Path, journal: &mut UndoJournal) -> Result<(), String> {
    loop {
        let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
        if bytes.len() <= UNDO_LIMIT && journal.entries.len() <= 32 {
            return atomic_write(path, &bytes, true, None);
        }
        if journal.entries.len() <= 1 {
            return Err("undo entry exceeds storage limit".into());
        }
        journal.entries.remove(0);
    }
}

/// Save recovery data before replacing a file. The operation lock serializes
/// edits and undo across processes; the file writer separately checks content.
pub fn write_with_backup(
    root: &Path,
    path: &Path,
    data: &[u8],
    original: Option<&[u8]>,
) -> Result<(), String> {
    write_with_journal(root, path, data, original, &undo_path(root)?)
}

fn write_with_journal(
    root: &Path,
    path: &Path,
    data: &[u8],
    original: Option<&[u8]>,
    journal_path: &Path,
) -> Result<(), String> {
    let _lock = lock_file(&journal_path.with_extension("active.lock"))?;
    let mut journal = load_undo(journal_path, root)?;
    let previous = journal.clone();
    journal.entries.push(BackupEntry {
        path: path.to_path_buf(),
        original: original.map(Vec::from),
        written: data.to_vec(),
    });
    save_undo(journal_path, &mut journal)?;
    if let Err(error) = atomic_write_project(root, path, data, Some(original)) {
        // A directory-sync error can occur after replacement: retain recovery
        // data if the intended bytes were actually written.
        if !error.starts_with("syncing")
            && optional_read(path, FILE_LIMIT).is_ok_and(|current| current.as_deref() != Some(data))
        {
            journal = previous;
            save_undo(journal_path, &mut journal)
                .map_err(|e| format!("{error}; preserving undo history failed: {e}"))?;
        }
        return Err(error);
    }
    Ok(())
}

pub fn backup_count(root: &Path) -> Result<usize, String> {
    Ok(load_undo(&undo_path(root)?, root)?.entries.len())
}

pub fn undo_last_change(root: &Path) -> Result<String, String> {
    undo_from_journal(root, &undo_path(root)?)
}

fn undo_from_journal(root: &Path, journal_path: &Path) -> Result<String, String> {
    let _lock = lock_file(&journal_path.with_extension("active.lock"))?;
    let mut journal = load_undo(journal_path, root)?;
    let entry = journal
        .entries
        .last()
        .ok_or("No file changes in history to undo.")?;
    let input = entry.path.to_str().ok_or("undo path is not valid UTF-8")?;
    let checked = super::resolve_project_path(root, input, false)?;
    if checked != entry.path {
        return Err("undo path changed".into());
    }
    let current = optional_read(&entry.path, FILE_LIMIT)?;
    // Already-restored content makes recovery idempotent after a crash between
    // restoring a file and updating its journal.
    if current.as_deref() != entry.original.as_deref() {
        if current.as_deref() != Some(entry.written.as_slice()) {
            return Err(format!(
                "'{}' changed after Nio's edit; undo refused to protect your later changes. Recovery history was kept.",
                entry.path.display()
            ));
        }
        replace_project_file(
            root,
            &entry.path,
            entry.original.as_deref(),
            Some(Some(&entry.written)),
        )?;
    }
    let message = if entry.original.is_some() {
        format!("Restored '{}'", entry.path.display())
    } else {
        format!("Deleted newly created file '{}'", entry.path.display())
    };
    journal.entries.pop();
    save_undo(journal_path, &mut journal)?;
    Ok(message)
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
        atomic_write, atomic_write_project, optional_read, preview_for_path, read_bounded,
        trim_history,
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
        let diff = preview_for_path("file.txt", b"same\nold\n", "same\nnew\n");
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
        let patched =
            super::apply_patch(file, "    println!(\"hello\");", "    println!(\"world\");")
                .unwrap();
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
        let root = root.canonicalize().unwrap();
        let file_path = root.join("test.txt");
        std::fs::write(&file_path, b"original content").unwrap();

        let journal = root.join("undo.json");
        super::write_with_journal(
            &root,
            &file_path,
            b"modified content",
            Some(b"original content"),
            &journal,
        )
        .unwrap();
        let result = super::undo_from_journal(&root, &journal).unwrap();
        assert!(result.contains("Restored"));
        assert_eq!(std::fs::read(&file_path).unwrap(), b"original content");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn undo_preserves_later_edits_and_can_retry_from_persisted_history() {
        let root = temp_path("undo_conflict");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let file = root.join("file.txt");
        let journal = root.join("undo.json");
        std::fs::write(&file, b"original").unwrap();
        super::write_with_journal(&root, &file, b"agent", Some(b"original"), &journal).unwrap();
        std::fs::write(&file, b"user edit").unwrap();
        assert!(
            super::undo_from_journal(&root, &journal)
                .unwrap_err()
                .contains("later changes")
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"user edit");
        assert_eq!(super::load_undo(&journal, &root).unwrap().entries.len(), 1);
        std::fs::write(&file, b"agent").unwrap();
        super::undo_from_journal(&root, &journal).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"original");
        assert!(
            super::load_undo(&journal, &root)
                .unwrap()
                .entries
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_created_file_checks_content_and_failed_writes_keep_history() {
        let root = temp_path("undo_create");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let file = root.join("new.txt");
        let journal = root.join("undo.json");
        super::write_with_journal(&root, &file, b"agent", None, &journal).unwrap();
        assert!(super::write_with_journal(&root, &file, b"bad", Some(b"stale"), &journal).is_err());
        assert_eq!(super::load_undo(&journal, &root).unwrap().entries.len(), 1);
        std::fs::write(&file, b"later").unwrap();
        assert!(super::undo_from_journal(&root, &journal).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"later");
        std::fs::write(&file, b"agent").unwrap();
        super::undo_from_journal(&root, &journal).unwrap();
        assert!(!file.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_history_is_project_bound_and_idempotent_after_restore() {
        let root = temp_path("undo_binding");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let file = root.join("file.txt");
        let journal = root.join("undo.json");
        std::fs::write(&file, b"original").unwrap();
        super::write_with_journal(&root, &file, b"agent", Some(b"original"), &journal).unwrap();
        assert!(super::load_undo(&journal, &root.join("other")).is_err());
        std::fs::write(&file, b"original").unwrap();
        super::undo_from_journal(&root, &journal).unwrap();
        assert!(
            super::load_undo(&journal, &root)
                .unwrap()
                .entries
                .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_storage_caps_entries_and_serialized_bytes() {
        let root = temp_path("undo_bounds");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("undo.json");
        let entry = super::BackupEntry {
            path: root.join("file.txt"),
            original: Some(b"before".to_vec()),
            written: b"after".to_vec(),
        };
        let mut journal = super::UndoJournal {
            version: 1,
            root: root.clone(),
            entries: vec![entry.clone(); 40],
        };
        super::save_undo(&path, &mut journal).unwrap();
        assert_eq!(super::load_undo(&path, &root).unwrap().entries.len(), 32);
        let large = super::BackupEntry {
            original: Some(vec![255; super::FILE_LIMIT]),
            written: vec![255; super::FILE_LIMIT],
            ..entry
        };
        journal.entries = vec![large; 3];
        super::save_undo(&path, &mut journal).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= super::UNDO_LIMIT as u64);
        assert!(!super::load_undo(&path, &root).unwrap().entries.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn undo_rejects_replaced_parent_symlink_and_keeps_entry() {
        use std::os::unix::fs::symlink;
        let root = temp_path("undo_symlink");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root = root.canonicalize().unwrap();
        let file = root.join("sub/new.txt");
        let journal = root.join("undo.json");
        super::write_with_journal(&root, &file, b"agent", None, &journal).unwrap();
        std::fs::rename(root.join("sub"), root.join("moved")).unwrap();
        symlink(root.join("moved"), root.join("sub")).unwrap();
        assert!(super::undo_from_journal(&root, &journal).is_err());
        assert_eq!(std::fs::read(root.join("moved/new.txt")).unwrap(), b"agent");
        assert_eq!(super::load_undo(&journal, &root).unwrap().entries.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
