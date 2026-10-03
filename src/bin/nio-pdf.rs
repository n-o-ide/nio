//! Separately distributed PDF reader. Never linked into the default nio binary.
#[path = "../plugin_process.rs"]
mod plugin_process;
use serde::Deserialize;
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE_LIMIT: usize = 512 * 1024;
const DOCUMENT_LIMIT: usize = 20 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: u32,
    operation: String,
    path: PathBuf,
    data_dir: PathBuf,
    #[serde(default)]
    languages: Vec<String>,
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("nio-pdf {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let result = async {
        let mut input = Vec::new();
        std::io::stdin()
            .take(16 * 1024 + 1)
            .read_to_end(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() > 16 * 1024 {
            return Err("plugin request exceeds 16 KiB".into());
        }
        let request: Request =
            serde_json::from_slice(&input).map_err(|e| format!("invalid plugin request: {e}"))?;
        read_pdf(&request).await
    }
    .await;
    let response = match result {
        Ok(text) => json!({"protocol":1,"text":text}),
        Err(error) => json!({"protocol":1,"error":error}),
    };
    println!("{response}");
}

fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return Err("PDF input must be a regular file within its size limit".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("PDF input is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("PDF exceeds its size limit".into());
    }
    Ok(bytes)
}

struct LimitedPdfText(Vec<u8>);
impl Write for LimitedPdfText {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > FILE_LIMIT {
            return Err(std::io::Error::other("PDF text exceeds the 512 KiB limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn temporary() -> Result<Temporary, String> {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nio-pdf-{}-{id}", std::process::id()));
    std::fs::create_dir(&path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(Temporary(path))
}

fn add_text(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > FILE_LIMIT {
        return Err("PDF text exceeds the 512 KiB limit; split into smaller documents".into());
    }
    output.push_str(text);
    Ok(())
}

async fn read_pdf(request: &Request) -> Result<String, String> {
    if request.protocol != 1
        || request.operation != "read_file"
        || !request.path.is_absolute()
        || !request.data_dir.is_absolute()
    {
        return Err("PDF plugin requires protocol 1, read_file, and absolute paths".into());
    }
    if request.languages.len() > 8
        || request.languages.iter().any(|l| {
            l.is_empty()
                || l.len() > 40
                || l == "osd"
                || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    {
        return Err(
            "select at most 8 valid OCR languages; all installed models are not used at once"
                .into(),
        );
    }
    let bytes = bounded_read(&request.path, DOCUMENT_LIMIT)?;
    if !bytes.starts_with(b"%PDF-") {
        return Err("file does not have a PDF signature".into());
    }
    let mut document =
        pdf_extract::Document::load_mem(&bytes).map_err(|e| format!("cannot read PDF: {e}"))?;
    if document.is_encrypted() {
        document
            .decrypt("")
            .map_err(|_| "password-protected PDFs require an unlocked copy".to_string())?;
    }
    let pages = document.get_pages();
    if pages.len() > 1000 {
        return Err("PDF exceeds the 1000 page limit".into());
    }
    let mut output = String::new();
    let mut scanned = 0usize;
    let mut working = None;
    for page in pages.keys() {
        let mut writer = LimitedPdfText(Vec::new());
        let sink: &mut dyn Write = &mut writer;
        let mut dev = pdf_extract::PlainTextOutput::new(sink);
        pdf_extract::output_doc_page(&document, &mut dev, *page)
            .map_err(|e| format!("cannot extract PDF page {page}: {e}"))?;
        let mut text = String::from_utf8(writer.0).map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            scanned += 1;
            if request.languages.is_empty() {
                return Err(format!(
                    "PDF page {page} has no text layer. Enable optional OCR with nio --plugins install pdf --languages eng (or chosen language codes/all). OCR needs tesseract and pdftoppm on PATH."
                ));
            }
            if scanned > 50 {
                return Err(
                    "PDF exceeds the 50 OCR-page limit; split into smaller documents".into(),
                );
            }
            if working.is_none() {
                let temp = temporary()?;
                // Render a snapshot, so later source changes cannot alter OCR midway.
                std::fs::write(temp.0.join("input.pdf"), &bytes).map_err(|e| e.to_string())?;
                working = Some(temp);
            }
            text = ocr_page(request, *page, &working.as_ref().unwrap().0).await?;
            add_text(
                &mut output,
                &format!("\nPage {page} (OCR: {})\n", request.languages.join(",")),
            )?;
        } else {
            add_text(&mut output, &format!("\nPage {page}\n"))?;
        }
        add_text(&mut output, &text)?;
        add_text(&mut output, "\n")?;
    }
    if output.trim().is_empty() {
        return Err("PDF contains no readable pages".into());
    }
    Ok(output)
}

async fn ocr_page(request: &Request, page: u32, temporary: &Path) -> Result<String, String> {
    let tessdata = request.data_dir.join("tessdata");
    for language in &request.languages {
        if !tessdata.join(format!("{language}.traineddata")).is_file() {
            return Err(format!(
                "OCR model {language} is missing; install it with nio --plugins install pdf --languages {language}"
            ));
        }
    }
    let prefix = temporary.join("page");
    let mut renderer = tokio::process::Command::new("pdftoppm");
    renderer
        .args([
            "-f",
            &page.to_string(),
            "-l",
            &page.to_string(),
            "-singlefile",
            "-scale-to",
            "2400",
            "-r",
            "150",
            "-png",
        ])
        .arg(temporary.join("input.pdf"))
        .arg(&prefix)
        .current_dir(temporary);
    let rendered = plugin_process::run(
        &mut renderer,
        &[],
        8192,
        Duration::from_secs(60),
        None,
        false,
    )
    .await
    .map_err(|e| format!("PDF rendering needs Poppler's pdftoppm on PATH: {e}"))?;
    if !rendered.success {
        return Err(format!(
            "PDF page rendering failed: {}",
            String::from_utf8_lossy(&rendered.stderr)
        ));
    }
    let image = prefix.with_extension("png");
    let _ = bounded_read(&image, 32 * 1024 * 1024)?;
    let mut tesseract = tokio::process::Command::new("tesseract");
    tesseract
        .arg(&image)
        .args(["stdout", "--tessdata-dir"])
        .arg(tessdata)
        .args([
            "-l",
            &request.languages.join("+"),
            "--oem",
            "1",
            "--psm",
            "3",
        ])
        .current_dir(temporary);
    let recognized = plugin_process::run(
        &mut tesseract,
        &[],
        FILE_LIMIT,
        Duration::from_secs(60),
        None,
        false,
    )
    .await
    .map_err(|e| format!("PDF OCR needs Tesseract on PATH: {e}"))?;
    if !recognized.success {
        return Err(format!(
            "Tesseract OCR failed: {}",
            String::from_utf8_lossy(&recognized.stderr)
        ));
    }
    let text = String::from_utf8(recognized.stdout).map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        Ok("[No text recognized on this page.]".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pdf(text: Option<&str>) -> Vec<u8> {
        use pdf_extract::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let font = doc.add_object(
            dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
        );
        let resources = doc.add_object(dictionary! {"Font" => dictionary! {"F1" => font}});
        let content = text
            .map(|t| format!("BT /F1 12 Tf 50 700 Td ({t}) Tj ET"))
            .unwrap_or_default();
        let stream = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page = doc.add_object(dictionary! {"Type" => "Page", "Parent" => pages, "Contents" => stream, "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()]});
        doc.objects.insert(
            pages,
            Object::Dictionary(
                dictionary! {"Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1},
            ),
        );
        let catalog = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages});
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn text_pdf_and_scanned_pdf_without_languages() {
        let temp = temporary().unwrap();
        let path = temp.0.join("input.pdf");
        std::fs::write(&path, pdf(Some("Hello PDF"))).unwrap();
        let request = Request {
            protocol: 1,
            operation: "read_file".into(),
            path: path.clone(),
            data_dir: temp.0.clone(),
            languages: vec![],
        };
        let text = read_pdf(&request).await.unwrap();
        assert!(text.contains("Page 1"));
        assert!(text.contains("Hello PDF"));
        std::fs::write(&path, pdf(None)).unwrap();
        assert!(
            read_pdf(&request)
                .await
                .unwrap_err()
                .contains("--languages")
        );
        std::fs::write(&path, b"not a pdf").unwrap();
        assert!(read_pdf(&request).await.is_err());
    }

    #[test]
    fn pdf_output_limit_is_enforced() {
        let mut writer = LimitedPdfText(Vec::new());
        assert!(writer.write_all(&vec![0; FILE_LIMIT + 1]).is_err());
    }
}
