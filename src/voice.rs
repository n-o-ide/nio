//! Voice recording and speech-to-text / multimodal audio subsystem for NioAI.
use base64::Engine as _;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub fn voice_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("cannot determine user home directory")?;
    let path = PathBuf::from(home).join(".nio").join("voice");
    std::fs::create_dir_all(&path).map_err(|e| format!("creating voice dir: {e}"))?;
    Ok(path)
}

pub fn supported_audio_mime(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("wav") => Some("audio/wav"),
        Some("mp3") => Some("audio/mp3"),
        Some("m4a") => Some("audio/m4a"),
        Some("ogg") => Some("audio/ogg"),
        Some("flac") => Some("audio/flac"),
        Some("webm") => Some("audio/webm"),
        _ => None,
    }
}

pub enum AudioRecorderChild {
    SwiftStdin(Child),
    Process(Child),
}

fn start_recorder(output_path: &Path) -> Result<AudioRecorderChild, String> {
    let output_str = output_path.to_string_lossy();

    // macOS native Swift + AVFoundation recording
    #[cfg(target_os = "macos")]
    {
        if Command::new("swift").arg("-version").output().is_ok() {
            let swift_code = r#"
import Foundation
import AVFoundation

let args = CommandLine.arguments
let outputPath = args.count > 1 ? args[1] : "/tmp/nio_voice.wav"
let outputUrl = URL(fileURLWithPath: outputPath)

let settings: [String: Any] = [
    AVFormatIDKey: Int(kAudioFormatLinearPCM),
    AVSampleRateKey: 16000.0,
    AVNumberOfChannelsKey: 1,
    AVLinearPCMBitDepthKey: 16,
    AVLinearPCMIsFloatKey: false,
    AVLinearPCMIsBigEndianKey: false
]

do {
    let recorder = try AVAudioRecorder(url: outputUrl, settings: settings)
    recorder.record()
    print("READY")
    fflush(stdout)
    _ = readLine()
    recorder.stop()
    print("DONE")
} catch {
    fputs("Error: \(error)\n", stderr)
    exit(1)
}
"#;
            let mut cmd = Command::new("swift");
            cmd.arg("-e")
                .arg(swift_code)
                .arg(output_str.as_ref())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let child = cmd
                .spawn()
                .map_err(|e| format!("spawning Swift recorder: {e}"))?;
            return Ok(AudioRecorderChild::SwiftStdin(child));
        }
    }

    // Check for standard CLI recording tools (Linux / Unix / macOS fallback)
    if Command::new("arecord").arg("--version").output().is_ok() {
        let child = Command::new("arecord")
            .args(["-q", "-f", "S16_LE", "-r", "16000", "-c", "1"])
            .arg(output_str.as_ref())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning arecord: {e}"))?;
        return Ok(AudioRecorderChild::Process(child));
    }

    if Command::new("rec").arg("--version").output().is_ok() {
        let child = Command::new("rec")
            .args(["-q", "-r", "16000", "-c", "1"])
            .arg(output_str.as_ref())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning rec (sox): {e}"))?;
        return Ok(AudioRecorderChild::Process(child));
    }

    if Command::new("sox").arg("--version").output().is_ok() {
        let child = Command::new("sox")
            .args(["-q", "-d", "-r", "16000", "-c", "1"])
            .arg(output_str.as_ref())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning sox: {e}"))?;
        return Ok(AudioRecorderChild::Process(child));
    }

    if Command::new("ffmpeg").arg("-version").output().is_ok() {
        #[cfg(target_os = "macos")]
        let input_device = ":0";
        #[cfg(not(target_os = "macos"))]
        let input_device = "default";

        #[cfg(target_os = "macos")]
        let input_fmt = "avfoundation";
        #[cfg(not(target_os = "macos"))]
        let input_fmt = "alsa";

        let child = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                input_fmt,
                "-i",
                input_device,
                "-ar",
                "16000",
                "-ac",
                "1",
            ])
            .arg(output_str.as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning ffmpeg recorder: {e}"))?;
        return Ok(AudioRecorderChild::SwiftStdin(child));
    }

    Err("no supported audio recording tool found (requires 'swift' on macOS, or 'arecord'/'sox'/'ffmpeg' on Linux)".into())
}

fn stop_recorder(mut recorder: AudioRecorderChild) {
    match &mut recorder {
        AudioRecorderChild::SwiftStdin(child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = writeln!(stdin);
                let _ = stdin.flush();
            }
            let _ = child.wait();
        }
        AudioRecorderChild::Process(child) => {
            #[cfg(unix)]
            {
                let pid = child.id() as libc::pid_t;
                unsafe {
                    libc::kill(pid, libc::SIGINT);
                }
            }
            let _ = child.wait();
        }
    }
}

fn cancel_recorder(mut recorder: AudioRecorderChild, output_path: &Path) {
    match &mut recorder {
        AudioRecorderChild::SwiftStdin(child) | AudioRecorderChild::Process(child) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let _ = std::fs::remove_file(output_path);
}

/// Interactive microphone recording with live visual duration timer.
/// Press Enter or Space to stop and return the recorded WAV file path.
/// Press Esc or Ctrl+C to cancel.
pub fn record_voice_interactive() -> Result<Option<PathBuf>, String> {
    let dir = voice_dir()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let output_path = dir.join(format!("voice_{timestamp}.wav"));

    let recorder = start_recorder(&output_path)?;

    // Wait a brief moment for the recorder to initialize
    std::thread::sleep(Duration::from_millis(200));

    let started = Instant::now();
    let mut stdout = io::stdout();

    terminal::enable_raw_mode().map_err(|e| format!("enabling raw mode for recording: {e}"))?;

    let recording_result: Result<bool, String> = loop {
        let elapsed = started.elapsed();
        let secs = elapsed.as_secs();
        let mins = secs / 60;
        let rem_secs = secs % 60;

        let dot = if (elapsed.as_millis() / 500) % 2 == 0 {
            "🔴"
        } else {
            "⚪"
        };
        print!(
            "\r\x1b[2K{dot} \x1b[1;31mRecording voice...\x1b[0m [Enter / Space to finish · Esc to cancel]  \x1b[36m({:02}:{:02})\x1b[0m",
            mins, rem_secs
        );
        let _ = stdout.flush();

        if event::poll(Duration::from_millis(80)).map_err(|e| e.to_string())? {
            let event = event::read().map_err(|e| e.to_string())?;
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        break Ok(true);
                    }
                    KeyCode::Esc => {
                        break Ok(false);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break Ok(false);
                    }
                    _ => {}
                }
            }
        }
    };

    let _ = terminal::disable_raw_mode();
    println!();

    match recording_result {
        Ok(true) => {
            stop_recorder(recorder);
            // Verify recorded file exists and has content
            if let Ok(meta) = std::fs::metadata(&output_path) {
                if meta.len() > 1000 {
                    return Ok(Some(output_path));
                }
            }
            Err("recording produced an empty or truncated audio file".into())
        }
        _ => {
            cancel_recorder(recorder, &output_path);
            Ok(None)
        }
    }
}

#[allow(dead_code)]
pub fn encode_audio_base64(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading audio file: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Transcribes audio using an OpenAI-compatible speech-to-text endpoint (`/v1/audio/transcriptions`).
pub async fn transcribe_audio(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    wav_path: &Path,
) -> Result<String, String> {
    let file_bytes =
        std::fs::read(wav_path).map_err(|e| format!("reading audio for transcription: {e}"))?;
    let file_name = wav_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "voice.wav".into());

    let endpoints = [
        format!("{}/audio/transcriptions", base_url.trim_end_matches('/')),
        "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
        "https://api.openai.com/v1/audio/transcriptions".to_string(),
    ];

    let mut last_error = "no transcription endpoints available".to_string();

    for endpoint in endpoints {
        let (endpoint_url, key_to_use, model_name) = if endpoint.contains("groq.com") {
            let groq_key = std::env::var("GROQ_API_KEY")
                .ok()
                .or_else(|| api_key.map(str::to_string));
            (endpoint, groq_key, "whisper-large-v3")
        } else if endpoint.contains("openai.com") {
            let openai_key = std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| api_key.map(str::to_string));
            (endpoint, openai_key, "whisper-1")
        } else {
            (endpoint, api_key.map(str::to_string), "whisper-1")
        };

        let file_part = reqwest::multipart::Part::bytes(file_bytes.clone())
            .file_name(file_name.clone())
            .mime_str("audio/wav")
            .map_err(|e| e.to_string())?;

        let form = reqwest::multipart::Form::new()
            .text("model", model_name)
            .part("file", file_part);

        let mut req = client.post(&endpoint_url).multipart(form);
        if let Some(key) = key_to_use.as_deref() {
            if !key.trim().is_empty() {
                req = req.bearer_auth(key);
            }
        }

        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(text) = json["text"].as_str() {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                return Ok(trimmed.to_string());
                            }
                        }
                    }
                } else {
                    last_error = format!("HTTP {}", resp.status());
                }
            }
            Err(e) => {
                last_error = e.to_string();
            }
        }
    }

    Err(format!("speech transcription failed: {last_error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_audio_formats() {
        assert_eq!(
            supported_audio_mime(Path::new("speech.wav")),
            Some("audio/wav")
        );
        assert_eq!(
            supported_audio_mime(Path::new("recording.mp3")),
            Some("audio/mp3")
        );
        assert_eq!(
            supported_audio_mime(Path::new("voice.m4a")),
            Some("audio/m4a")
        );
        assert_eq!(
            supported_audio_mime(Path::new("track.ogg")),
            Some("audio/ogg")
        );
        assert_eq!(
            supported_audio_mime(Path::new("audio.flac")),
            Some("audio/flac")
        );
        assert_eq!(
            supported_audio_mime(Path::new("note.webm")),
            Some("audio/webm")
        );
        assert_eq!(supported_audio_mime(Path::new("file.txt")), None);
        assert_eq!(supported_audio_mime(Path::new("image.png")), None);
    }

    #[test]
    fn creates_and_accesses_voice_dir() {
        let dir = voice_dir().expect("voice directory should be valid");
        assert!(dir.exists());
    }

    #[test]
    fn encodes_audio_data_base64() {
        let temp_file = std::env::temp_dir().join("test_nio_audio.wav");
        std::fs::write(&temp_file, b"RIFF....WAVEfmt ").unwrap();
        let encoded = encode_audio_base64(&temp_file).unwrap();
        assert!(!encoded.is_empty());
        let _ = std::fs::remove_file(temp_file);
    }
}
