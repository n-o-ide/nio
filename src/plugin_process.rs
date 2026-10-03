//! Bounded, timed subprocess communication for executable plugins.
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
}

async fn read_output(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("plugin process output exceeds its limit".into());
    }
    Ok(bytes)
}

struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // The caller created this process group, containing only its worker.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

pub async fn run(
    command: &mut tokio::process::Command,
    input: &[u8],
    limit: usize,
    timeout: Duration,
    cancelled: Option<Arc<AtomicBool>>,
    own_group: bool,
) -> Result<Output, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    if own_group {
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting plugin process: {e}"))?;
    let group = ProcessGroup(if own_group { child.id() } else { None });
    let mut stdin = child.stdin.take().ok_or("missing process stdin")?;
    let stdout = child.stdout.take().ok_or("missing process stdout")?;
    let stderr = child.stderr.take().ok_or("missing process stderr")?;
    let operation = async {
        let write = async {
            stdin.write_all(input).await.map_err(|e| e.to_string())?;
            drop(stdin);
            Ok::<(), String>(())
        };
        let wait = async { child.wait().await.map_err(|e| e.to_string()) };
        let (_, stdout, stderr, status) = tokio::try_join!(
            write,
            read_output(stdout, limit),
            read_output(stderr, 8192),
            wait
        )?;
        Ok(Output {
            stdout,
            stderr,
            success: status.success(),
        })
    };
    let cancellation = async {
        loop {
            if cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let result = tokio::select! {
        result = operation => result,
        _ = tokio::time::sleep(timeout) => Err("plugin process timed out".into()),
        _ = cancellation => Err("plugin operation cancelled".into()),
    };
    drop(group);
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn process_output_timeout_and_cancellation_are_enforced() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "printf abcdef"]);
        assert!(
            run(&mut command, &[], 3, Duration::from_secs(5), None, true)
                .await
                .is_err()
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 10"]);
        let start = std::time::Instant::now();
        assert!(
            run(
                &mut command,
                &[],
                100,
                Duration::from_millis(50),
                None,
                true
            )
            .await
            .err()
            .unwrap()
            .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 10"]);
        assert!(
            run(
                &mut command,
                &[],
                100,
                Duration::from_secs(5),
                Some(Arc::new(AtomicBool::new(true))),
                true
            )
            .await
            .err()
            .unwrap()
            .contains("cancelled")
        );
    }
}
