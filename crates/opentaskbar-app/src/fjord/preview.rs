//! Cancellable file previews (`FjordFilePreview`): images, PDF first page,
//! bounded text and friendly fallbacks.
//!
//! The pure helpers above `@pure-end` have no dependencies outside `std` and
//! are unit-tested. WinRT `Windows.Data.Pdf` is intentionally not shimmed: the
//! workspace `windows` feature list does not include `Data_Pdf`, so PDFs expose
//! their page count and a "first-page rendering requires Data_Pdf" status until
//! the orchestrator enables that feature.

use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::files::FileEntry;

// @pure-start
/// `FjordFilePreview.MaxTextBytes`.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Upper bound for image bytes read into memory.
pub const MAX_IMAGE_BYTES: usize = 24 * 1024 * 1024;
/// Only the PDF header is parsed for page metadata.
pub const MAX_PDF_HEADER: usize = 64 * 1024;

const IMAGE_EXTENSIONS: [&str; 8] = [
    ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".tif", ".tiff", ".ico",
];
const TEXT_EXTENSIONS: [&str; 18] = [
    ".txt", ".md", ".json", ".csv", ".log", ".ini", ".xml", ".yaml", ".yml", ".cs", ".py", ".js",
    ".css", ".html", ".ps1", ".m", ".ts", ".tex",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Folder,
    Image,
    Pdf,
    Text,
    Unsupported,
}

/// Extension classifier, case-insensitive like the `HashSet`s.
pub fn classify(name: &str, is_folder: bool) -> PreviewKind {
    if is_folder {
        return PreviewKind::Folder;
    }
    let extension = Path::new(name)
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        PreviewKind::Image
    } else if extension == ".pdf" {
        PreviewKind::Pdf
    } else if TEXT_EXTENSIONS.contains(&extension.as_str()) {
        PreviewKind::Text
    } else {
        PreviewKind::Unsupported
    }
}

/// What the preview pane should render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    NothingSelected,
    Folder { name: String },
    Image { width: u32, height: u32 },
    Pdf { pages: u32 },
    Text { text: String, truncated: bool },
    Unsupported,
    Failed,
    Cancelled,
}

impl Preview {
    /// Locale key used by the GPUI surface.
    pub fn status_key(&self) -> Option<&'static str> {
        match self {
            Preview::NothingSelected => Some("SelectPreview"),
            Preview::Unsupported => Some("UnsupportedPreview"),
            Preview::Failed => Some("PreviewFailed"),
            Preview::Cancelled => Some("PreviewDisabledHint"),
            Preview::Pdf { .. } => Some("PdfPages"),
            Preview::Text {
                truncated: true, ..
            } => Some("TextLimit"),
            _ => None,
        }
    }
}

/// Reads at most `max` bytes as text, detecting a UTF-8 BOM and reporting
/// whether more bytes remain. Mirrors `FjordFilePreview.ReadText`.
pub fn read_text_limited(data: &[u8], max: usize) -> (String, bool) {
    let mut taken = data.len().min(max);
    let truncated = data.len() > taken;
    if truncated {
        // Drop trailing continuation bytes so a multi-byte character is never cut in half.
        while taken > 0 && data[taken - 1] & 0xC0 == 0x80 {
            taken -= 1;
        }
    }
    let mut bytes = &data[..taken];
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes = &bytes[3..];
    }
    let text = String::from_utf8_lossy(bytes).into_owned();
    (text.replace('\0', "\u{fffd}"), truncated)
}

/// Reads `/Count N` from a truncated PDF header. Best effort: many PDFs use
/// object streams, in which case `None` is returned rather than guessing.
pub fn pdf_page_count(header: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(header);
    let mut rest = text.as_ref();
    let mut best = None;
    while let Some(index) = rest.find("/Count") {
        let tail = &rest[index + 6..];
        let digits: String = tail
            .trim_start_matches(|c: char| c.is_ascii_whitespace())
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(count) = digits.parse::<u32>()
            && count > 0
            && count < 100_000
        {
            best = Some(count);
        }
        rest = tail;
    }
    best
}

/// Pure cancellation probe shared by every read path.
pub fn cancelled(token: &AtomicBool) -> bool {
    token.load(Ordering::SeqCst)
}
// @pure-end

/// Builds a preview for one entry. The caller may drop this call by setting the
/// cancellation flag; every I/O step re-checks it.
pub fn build(entry: Option<&FileEntry>, token: &AtomicBool) -> Preview {
    let Some(entry) = entry else {
        return Preview::NothingSelected;
    };
    if cancelled(token) {
        return Preview::Cancelled;
    }
    if entry.is_folder {
        return Preview::Folder {
            name: entry.name.clone(),
        };
    }
    let path = Path::new(&entry.path);
    let kind = classify(&entry.name, false);
    let read = |limit: usize| -> Option<Vec<u8>> {
        use std::io::Read;
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    };

    match kind {
        PreviewKind::Image => {
            let Some(bytes) = read(MAX_IMAGE_BYTES) else {
                return Preview::Failed;
            };
            if cancelled(token) {
                return Preview::Cancelled;
            }
            // `into_dimensions` avoids decoding the full bitmap; enabled codecs
            // come from the workspace `image` dependency.
            match image::ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
                Ok(reader) => match reader.into_dimensions() {
                    Ok((width, height)) if !cancelled(token) => Preview::Image { width, height },
                    Ok(_) => Preview::Cancelled,
                    Err(_) => Preview::Failed,
                },
                Err(_) => Preview::Failed,
            }
        }
        PreviewKind::Pdf => {
            // NOTE(WS-orchestrator): first-page rasterisation needs the WinRT
            // `Windows.Data.Pdf` API (`PdfDocument.LoadFromFileAsync`). The
            // workspace does not enable `Data_Pdf`, so no local shim is used.
            let Some(header) = read(MAX_PDF_HEADER) else {
                return Preview::Failed;
            };
            if cancelled(token) {
                return Preview::Cancelled;
            }
            match pdf_page_count(&header) {
                Some(pages) => Preview::Pdf { pages },
                None => Preview::Unsupported,
            }
        }
        PreviewKind::Text => {
            let Some(bytes) = read(MAX_TEXT_BYTES) else {
                return Preview::Failed;
            };
            if cancelled(token) {
                return Preview::Cancelled;
            }
            let (text, truncated) = read_text_limited(&bytes, MAX_TEXT_BYTES);
            if text.is_empty() {
                return Preview::Text {
                    text: "EmptyFile".to_owned(),
                    truncated: false,
                };
            }
            Preview::Text { text, truncated }
        }
        PreviewKind::Folder | PreviewKind::Unsupported => Preview::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn classify_extensions_case_insensitively() {
        assert_eq!(classify("photo.PNG", false), PreviewKind::Image);
        assert_eq!(classify("notes.MD", false), PreviewKind::Text);
        assert_eq!(classify("report.PDF", false), PreviewKind::Pdf);
        assert_eq!(classify("app.exe", false), PreviewKind::Unsupported);
        assert_eq!(classify("folder", true), PreviewKind::Folder);
        // `Path.GetExtension(".txt")` is empty in reference too: a bare dotfile is unsupported.
        assert_eq!(classify(".txt", false), PreviewKind::Unsupported);
    }

    #[test]
    fn text_preview_is_bounded_and_reports_truncation() {
        let data = b"abcdef";
        assert_eq!(read_text_limited(data, 3), ("abc".to_owned(), true));
        assert_eq!(read_text_limited(data, 99), ("abcdef".to_owned(), false));
    }

    #[test]
    fn text_preview_handles_bom_nul_and_invalid_utf8() {
        let bytes = [0xEF, 0xBB, 0xBF, b'a', 0, 0xFF];
        let (text, truncated) = read_text_limited(&bytes, 64);
        assert!(!truncated);
        assert!(text.starts_with('a'));
        assert!(text.contains('\u{fffd}'));
        assert!(!text.contains('\0'));
    }

    #[test]
    fn text_preview_does_not_end_with_broken_utf8() {
        let bytes = [b'a', 0xC3];
        let (text, truncated) = read_text_limited(&bytes, 1);
        assert!(truncated);
        assert_eq!(text, "a");
    }

    #[test]
    fn pdf_page_count_reads_count_and_ignores_noise() {
        assert_eq!(
            pdf_page_count(b"%PDF-1.7 /Type /Catalog /Count 7 >>"),
            Some(7)
        );
        assert_eq!(pdf_page_count(b"%PDF-1.7 /Count 0 /Count 12"), Some(12));
        assert_eq!(pdf_page_count(b"%PDF-1.7 no page tree"), None);
    }

    #[test]
    fn cancelled_probe_and_build_failure_paths() {
        let token = AtomicBool::new(true);
        assert!(cancelled(&token));
        assert_eq!(build(None, &token), Preview::NothingSelected);
        let entry = FileEntry::new(r"C:\missing.txt", "missing.txt", false);
        assert_eq!(build(Some(&entry), &token), Preview::Cancelled);
        token.store(false, Ordering::SeqCst);
        assert_eq!(build(Some(&entry), &token), Preview::Failed);
    }

    #[test]
    fn folder_preview_needs_no_io() {
        let entry = FileEntry::new(r"C:\Users", "Users", true);
        assert_eq!(
            build(Some(&entry), &AtomicBool::new(false)),
            Preview::Folder {
                name: "Users".to_owned()
            }
        );
    }
}
