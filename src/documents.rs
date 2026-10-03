//! Read-only document extraction shared by attachments and read_file.
use crate::reliability::{FILE_LIMIT, read_bounded};
use calamine::{Reader, open_workbook_auto_from_rs};
use quick_xml::{Reader as XmlReader, events::Event};
use std::io::{Cursor, Read};
use std::path::Path;

pub const DOCUMENT_LIMIT: usize = 20 * 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 32 * 1024 * 1024;

fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}

pub fn is_document(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "docx"
            | "docm"
            | "pptx"
            | "pptm"
            | "odt"
            | "odp"
            | "xlsx"
            | "xlsm"
            | "xlam"
            | "xls"
            | "xlsb"
            | "ods"
    )
}

pub fn read_text(path: &Path) -> Result<String, String> {
    let ext = extension(path);
    if ext == "pdf" {
        return Err("PDF support is optional; install it with nio --plugins install pdf".into());
    }
    if ext == "doc" || ext == "ppt" {
        return Err(format!(
            "legacy .{ext} documents are not supported; save as .{ext}x first"
        ));
    }
    let bytes = read_bounded(
        path,
        if is_document(path) {
            DOCUMENT_LIMIT
        } else {
            FILE_LIMIT
        },
    )?;
    let text = match ext.as_str() {
        "xlsx" | "xlsm" | "xlam" | "xls" | "xlsb" | "ods" => spreadsheet(&bytes)?,
        "docx" | "docm" | "pptx" | "pptm" | "odt" | "odp" => office_text(&bytes, &ext)?,
        _ => decode_text(&bytes)?,
    };
    if text.len() > FILE_LIMIT {
        return Err(
            "extracted text exceeds the 512 KiB limit; split the document into smaller files"
                .into(),
        );
    }
    Ok(text)
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if !(bytes.len() - 2).is_multiple_of(2) {
            return Err("incomplete UTF-16 text".into());
        }
        let little = bytes[0] == 0xff;
        let units: Vec<_> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|e| format!("invalid UTF-16 text: {e}"))?
    } else {
        String::from_utf8(
            bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(bytes)
                .to_vec(),
        )
        .map_err(|_| {
            "unsupported binary file; use a supported document or UTF-8/UTF-16 text".to_string()
        })?
    };
    if text.contains('\0') {
        return Err("unsupported binary file (contains NUL bytes)".into());
    }
    Ok(text)
}

fn checked_push(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > FILE_LIMIT {
        return Err(
            "extracted text exceeds the 512 KiB limit; split the document into smaller files"
                .into(),
        );
    }
    output.push_str(text);
    Ok(())
}

fn check_archive(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("invalid document archive: {e}"))?;
    if archive.len() > 4096 {
        return Err("document archive has too many entries (limit 4096)".into());
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("cannot read document archive: {e}"))?;
        total = total.saturating_add(entry.size());
        if total > ARCHIVE_LIMIT {
            return Err("document archive exceeds the 32 MiB expanded size limit".into());
        }
    }
    Ok(archive)
}

fn spreadsheet(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(b"PK") {
        check_archive(bytes)?;
    }
    let mut book = open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| format!("cannot read spreadsheet: {e}"))?;
    let mut output = String::new();
    for name in book.sheet_names().to_owned() {
        checked_push(&mut output, &format!("\nSheet: {name}\n"))?;
        let range = book
            .worksheet_range(&name)
            .map_err(|e| format!("cannot read sheet {name}: {e}"))?;
        if let Some((_, column)) = range.start() {
            checked_push(
                &mut output,
                &format!("First column: {} (1-based)\n", column + 1),
            )?;
        }
        for (index, row) in range.rows().enumerate() {
            checked_push(
                &mut output,
                &format!(
                    "Row {}:\t",
                    range.start().map_or(0, |s| s.0 as usize) + index + 1
                ),
            )?;
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    checked_push(&mut output, "\t")?;
                }
                // Escape delimiters so multiline cells remain distinct in the extracted table.
                checked_push(
                    &mut output,
                    &cell
                        .to_string()
                        .replace('\\', "\\\\")
                        .replace('\t', "\\t")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r"),
                )?;
            }
            checked_push(&mut output, "\n")?;
        }
    }
    Ok(output)
}

fn office_text(bytes: &[u8], ext: &str) -> Result<String, String> {
    let mut archive = check_archive(bytes)?;
    let mut names: Vec<String> = match ext {
        "docx" | "docm" => vec!["word/document.xml".into()],
        "odt" | "odp" => vec!["content.xml".into()],
        _ => {
            let mut slides: Vec<(u32, String)> = archive
                .file_names()
                .filter_map(|name| {
                    let number = name
                        .strip_prefix("ppt/slides/slide")?
                        .strip_suffix(".xml")?
                        .parse()
                        .ok()?;
                    Some((number, name.into()))
                })
                .collect();
            slides.sort_by_key(|(number, _)| *number);
            slides.into_iter().map(|(_, name)| name).collect()
        }
    };
    // Include Word headers, footers and notes as separate sections.
    if matches!(ext, "docx" | "docm") {
        let mut extras: Vec<_> = archive
            .file_names()
            .filter(|name| {
                name.ends_with(".xml")
                    && (name.starts_with("word/header")
                        || name.starts_with("word/footer")
                        || matches!(*name, "word/footnotes.xml" | "word/endnotes.xml"))
            })
            .map(str::to_owned)
            .collect();
        extras.sort();
        names.extend(extras);
    }
    if names.is_empty() {
        return Err("document contains no readable sections".into());
    }
    let mut output = String::new();
    for name in names {
        let entry = archive
            .by_name(&name)
            .map_err(|e| format!("missing document section {name}: {e}"))?;
        let mut xml = Vec::new();
        entry
            .take(ARCHIVE_LIMIT + 1)
            .read_to_end(&mut xml)
            .map_err(|e| e.to_string())?;
        if xml.len() as u64 > ARCHIVE_LIMIT {
            return Err("document section exceeds expanded size limit".into());
        }
        checked_push(&mut output, &format!("\nSection: {name}\n"))?;
        xml_text(&xml, &mut output, matches!(ext, "odt" | "odp"))?;
    }
    Ok(output)
}

fn xml_text(xml: &[u8], output: &mut String, open_document: bool) -> Result<(), String> {
    let mut reader = XmlReader::from_reader(xml);
    let mut text_depth = 0usize;
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("invalid document XML: {e}"))?
        {
            Event::Start(e) => {
                let local = e.local_name();
                if local.as_ref() == b"t"
                    || (open_document && matches!(local.as_ref(), b"p" | b"h"))
                {
                    text_depth += 1;
                }
                if local.as_ref() == b"tab" {
                    checked_push(output, "\t")?;
                }
            }
            Event::End(e) => {
                let local = e.local_name();
                if local.as_ref() == b"t"
                    || (open_document && matches!(local.as_ref(), b"p" | b"h"))
                {
                    text_depth = text_depth.saturating_sub(1);
                }
                match local.as_ref() {
                    b"p" | b"h" | b"tr" | b"table-row" => checked_push(output, "\n")?,
                    b"tc" | b"table-cell" => checked_push(output, "\t")?,
                    _ => {}
                }
            }
            Event::Empty(e) => match e.local_name().as_ref() {
                b"tab" => checked_push(output, "\t")?,
                b"br" | b"line-break" => checked_push(output, "\n")?,
                b"s" if open_document => {
                    let count = e
                        .attributes()
                        .filter_map(Result::ok)
                        .find(|a| a.key.local_name().as_ref() == b"c")
                        .and_then(|a| std::str::from_utf8(&a.value).ok()?.parse::<usize>().ok())
                        .unwrap_or(1);
                    if count > FILE_LIMIT {
                        return Err("document whitespace exceeds extraction limit".into());
                    }
                    checked_push(output, &" ".repeat(count))?;
                }
                _ => {}
            },
            Event::Text(e) if text_depth > 0 => {
                let decoded = e.decode().map_err(|e| e.to_string())?;
                checked_push(output, &decoded)?;
            }
            Event::GeneralRef(e) if text_depth > 0 => {
                let reference = e.decode().map_err(|e| e.to_string())?;
                let escaped = format!("&{reference};");
                let decoded = quick_xml::escape::unescape(&escaped).map_err(|e| e.to_string())?;
                checked_push(output, &decoded)?;
            }
            Event::CData(e) if text_depth > 0 => {
                checked_push(output, &e.decode().map_err(|e| e.to_string())?)?
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(ext: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nio-doc-{}-{}.{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                ext
            ));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
        fn read(&self) -> Result<String, String> {
            read_text(&self.0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn text_formats_and_unicode_encodings() {
        for ext in [
            "md", "txt", "json", "csv", "yaml", "toml", "xml", "html", "log", "rs",
        ] {
            assert_eq!(
                Fixture::new(ext, "Hello 世界".as_bytes()).read().unwrap(),
                "Hello 世界"
            );
        }
        for little in [true, false] {
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in "Hello 世界".encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            assert_eq!(Fixture::new("txt", &bytes).read().unwrap(), "Hello 世界");
        }
        assert_eq!(
            Fixture::new("txt", b"\xef\xbb\xbfhello").read().unwrap(),
            "hello"
        );
        assert!(Fixture::new("txt", b"\xff\xfe\x00").read().is_err());
        assert!(Fixture::new("bin", b"abc\0xyz").read().is_err());
    }

    #[test]
    fn word_paragraphs_tables_entities_and_notes() {
        let bytes = archive(&[
            (
                "word/document.xml",
                "<w:document xmlns:w='word'><w:p><w:r><w:t>Hello &amp; 世界</w:t><w:tab/><w:t>friend</w:t><w:br/><w:t>next</w:t></w:r></w:p><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:document>",
            ),
            (
                "word/footnotes.xml",
                "<w:p xmlns:w='word'><w:t>note</w:t></w:p>",
            ),
        ]);
        let text = Fixture::new("DOCX", &bytes).read().unwrap();
        assert!(text.contains("Hello & 世界\tfriend\nnext\n"), "{text}");
        assert!(text.contains("cell\n\t\n"));
        assert!(text.contains("note"));
    }

    #[test]
    fn powerpoint_slides_use_numeric_order() {
        let bytes = archive(&[
            (
                "ppt/slides/slide10.xml",
                "<a:p xmlns:a='a'><a:t>tenth</a:t></a:p>",
            ),
            (
                "ppt/slides/slide2.xml",
                "<a:p xmlns:a='a'><a:t>second</a:t></a:p>",
            ),
        ]);
        let text = Fixture::new("pptx", &bytes).read().unwrap();
        assert!(text.find("second").unwrap() < text.find("tenth").unwrap());
    }

    #[test]
    fn open_document_preserves_inline_text_and_spaces() {
        let bytes = archive(&[(
            "content.xml",
            "<office xmlns:text='text'><text:h>Title</text:h><text:p>Hello<text:s text:c='3'/><text:span>world</text:span><text:line-break/>next</text:p></office>",
        )]);
        let text = Fixture::new("odt", &bytes).read().unwrap();
        assert!(text.contains("Title\nHello   world\nnext\n"), "{text}");
    }

    #[test]
    fn xlsx_preserves_sheet_row_column_and_cell_values() {
        let bytes = archive(&[
            (
                "[Content_Types].xml",
                "<Types xmlns='http://schemas.openxmlformats.org/package/2006/content-types'><Override PartName='/xl/workbook.xml' ContentType='application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml'/></Types>",
            ),
            (
                "xl/workbook.xml",
                "<workbook xmlns:r='http://schemas.openxmlformats.org/officeDocument/2006/relationships'><sheets><sheet name='Sales' sheetId='1' r:id='rId1'/></sheets></workbook>",
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id='rId1' Target='worksheets/sheet1.xml' Type='http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet'/></Relationships>",
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><dimension ref='B3:C4'/><sheetData><row r='3'><c r='B3' t='inlineStr'><is><t>Revenue</t></is></c><c r='C3'><v>42</v></c></row><row r='4'><c r='B4' t='inlineStr'><is><t>Profit</t></is></c><c r='C4'><f>C3/2</f><v>21</v></c></row></sheetData></worksheet>",
            ),
        ]);
        let text = Fixture::new("xlsx", &bytes).read().unwrap();
        assert!(text.contains("Sheet: Sales"));
        assert!(text.contains("First column: 2"));
        assert!(text.contains("Row 3:\tRevenue\t42"), "{text}");
        assert!(text.contains("Row 4:\tProfit\t21"));
    }

    #[test]
    fn malformed_archives_legacy_formats_and_limits() {
        assert!(Fixture::new("docx", b"not zip").read().is_err());
        assert!(
            Fixture::new("doc", b"legacy")
                .read()
                .unwrap_err()
                .contains("docx")
        );
        assert!(
            Fixture::new("ppt", b"legacy")
                .read()
                .unwrap_err()
                .contains("pptx")
        );
        assert!(Fixture::new("xlsx", b"not zip").read().is_err());
        assert!(
            Fixture::new("txt", &vec![b'a'; FILE_LIMIT + 1])
                .read()
                .is_err()
        );
        let bytes = archive(&[(
            "word/document.xml",
            &format!("<p><t>{}</t></p>", "a".repeat(FILE_LIMIT + 1)),
        )]);
        assert!(
            Fixture::new("docx", &bytes)
                .read()
                .unwrap_err()
                .contains("512 KiB")
        );
        let bytes = archive(&[(
            "content.xml",
            "<p xmlns:text='text'><text:s text:c='99999999'/></p>",
        )]);
        assert!(Fixture::new("odt", &bytes).read().is_err());
    }
}
