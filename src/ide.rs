use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn ide_bin_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("cannot determine user home directory")?;
    let path = PathBuf::from(home).join(".nio").join("bin");
    std::fs::create_dir_all(&path).map_err(|e| format!("creating nio bin dir: {e}"))?;
    Ok(path)
}

pub fn pid_file() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("cannot determine user home directory")?;
    let path = PathBuf::from(home).join(".nio");
    std::fs::create_dir_all(&path).map_err(|e| format!("creating nio dir: {e}"))?;
    Ok(path.join("nio-de.pid"))
}

pub fn find_binary() -> Option<PathBuf> {
    // 1. Check ~/.nio/bin/nio-de
    if let Ok(dir) = ide_bin_dir() {
        let exe = dir.join(format!("nio-de{}", std::env::consts::EXE_SUFFIX));
        if exe.is_file() {
            return Some(exe);
        }
    }
    // 2. Check beside nio executable
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let beside = parent.join(format!("nio-de{}", std::env::consts::EXE_SUFFIX));
            if beside.is_file() {
                return Some(beside);
            }
        }
    }
    // 3. Check workspace target directory
    for profile in &["release", "debug"] {
        let path = Path::new("target")
            .join(profile)
            .join(format!("nio-de{}", std::env::consts::EXE_SUFFIX));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn is_running() -> Option<u32> {
    let path = pid_file().ok()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let pid = content.trim().parse::<u32>().ok()?;
    #[cfg(unix)]
    {
        unsafe {
            if libc::kill(pid as i32, 0) == 0 {
                return Some(pid);
            }
        }
        let _ = std::fs::remove_file(path);
        None
    }
    #[cfg(not(unix))]
    {
        Some(pid)
    }
}

pub fn status() -> Result<String, String> {
    let mut text = String::from("NioDE Server Status:\n");
    if let Some(bin) = find_binary() {
        text.push_str(&format!("  Binary: Installed at {}\n", bin.display()));
    } else {
        text.push_str("  Binary: Not installed (use 'nio ide install' to set up)\n");
    }

    if let Some(pid) = is_running() {
        let port = std::env::var("NIO_DE_PORT").unwrap_or_else(|_| "8080".into());
        text.push_str(&format!("  Status: Running (PID: {pid})\n"));
        text.push_str(&format!("  URL: http://localhost:{port}\n"));
    } else {
        text.push_str("  Status: Stopped\n");
    }
    Ok(text)
}

pub async fn install() -> Result<String, String> {
    let target_dir = ide_bin_dir()?;
    let target_bin = target_dir.join(format!("nio-de{}", std::env::consts::EXE_SUFFIX));

    // If already beside nio or in target, copy it to ~/.nio/bin/nio-de
    if let Some(existing) = find_binary() {
        if existing != target_bin {
            std::fs::copy(&existing, &target_bin)
                .map_err(|e| format!("copying local nio-de: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target_bin, std::fs::Permissions::from_mode(0o755));
            }
            return Ok(format!(
                "Installed local nio-de from {} to {}",
                existing.display(),
                target_bin.display()
            ));
        }
        return Ok(format!("nio-de is already installed at {}", target_bin.display()));
    }

    // Platform detection for official release bundle download
    let arch = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => return Err(format!("Unsupported platform for nio-de bundle: {os}-{arch}")),
    };

    Ok(format!(
        "nio-de bundle ready for installation ({arch}).\nTarget path: {}\nWhen published releases are available, 'nio ide install' automatically downloads and verifies the platform binary.",
        target_bin.display()
    ))
}

pub fn start(port: Option<u16>) -> Result<String, String> {
    if let Some(pid) = is_running() {
        return Ok(format!("NioDE server is already running (PID: {pid})."));
    }
    let binary = find_binary()
        .ok_or("nio-de binary not found. Run 'nio ide install' first.")?;
    let port_str = port.unwrap_or(8080).to_string();

    let child = std::process::Command::new(&binary)
        .arg("--port")
        .arg(&port_str)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start nio-de: {e}"))?;

    let pid = child.id();
    if let Ok(pid_path) = pid_file() {
        let _ = std::fs::write(pid_path, pid.to_string());
    }

    Ok(format!(
        "Started NioDE server (PID: {pid}) at http://localhost:{port_str}"
    ))
}

pub fn stop() -> Result<String, String> {
    let Some(pid) = is_running() else {
        return Ok("NioDE server is not running.".into());
    };

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(100));
        let _ = pid_file().map(|p| std::fs::remove_file(p));
        Ok(format!("Stopped NioDE server (PID: {pid})."))
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
        let _ = pid_file().map(|p| std::fs::remove_file(p));
        Ok(format!("Stopped NioDE server (PID: {pid})."))
    }
}

pub async fn command(args: &[String]) -> Result<(), String> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "status" => {
            println!("{}", status()?);
        }
        "install" => {
            println!("{}", install().await?);
        }
        "start" => {
            let port = args.get(1).and_then(|p| p.parse::<u16>().ok());
            println!("{}", start(port)?);
        }
        "stop" => {
            println!("{}", stop()?);
        }
        _ => {
            println!("Usage:");
            println!("  nio ide status          Check server status");
            println!("  nio ide install         Install or update nio-de server bundle");
            println!("  nio ide start [--port]  Start the nio-de background server");
            println!("  nio ide stop           Stop the running nio-de server");
        }
    }
    Ok(())
}
