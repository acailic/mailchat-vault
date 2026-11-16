//! Pure PDF text extraction — attachment-content search, part 1a
//! ([#171](https://github.com/acailic/mailchat-vault/issues/171)).
//!
//! Turns PDF bytes into per-page text with local parsing only: no
//! network, no I/O, no document bytes leave the process, and the input
//! is never modified. The derived text is separate data *about* a
//! preserved object (D05); the indexing job, per-reference status, and
//! storage that record it arrive in the later parts of #171.
//!
//! Extraction is bounded by an explicit [`PdfExtractionBudget`] (input
//! size and page count), and every way a PDF can be unusable is an
//! explicit [`PdfTextError`] variant instead of a vague failure:
//! not-a-PDF input is [`PdfTextError::Unsupported`], password-protected
//! documents are [`PdfTextError::Encrypted`], broken files are
//! [`PdfTextError::Corrupt`], and oversized inputs are
//! [`PdfTextError::OverBudget`].

use pdf_extract::OutputError;

/// Bounds for one extraction attempt. Callers own the values; the
/// indexing job (a later part of #171) sets them for tracked,
/// cancellable work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfExtractionBudget {
    /// Largest accepted PDF input in bytes.
    pub max_bytes: usize,
    /// Largest accepted page count.
    pub max_pages: usize,
}

impl PdfExtractionBudget {
    pub const fn new(max_bytes: usize, max_pages: usize) -> Self {
        Self {
            max_bytes,
            max_pages,
        }
    }
}

/// Which budget dimension was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfBudgetDimension {
    Bytes,
    Pages,
}

impl std::fmt::Display for PdfBudgetDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PdfBudgetDimension::Bytes => "input size in bytes",
            PdfBudgetDimension::Pages => "page count",
        })
    }
}

/// Explicit failure modes for PDF text extraction. Derived text is
/// best-effort; these are the inputs that produce no text at all and
/// must be recorded as such by the caller.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PdfTextError {
    #[error("unsupported input: no PDF header")]
    Unsupported,
    #[error("encrypted PDF: a password is required and none is available")]
    Encrypted,
    #[error("corrupt PDF: {0}")]
    Corrupt(String),
    #[error("over budget: {dimension} {actual} exceeds the limit {limit}")]
    OverBudget {
        dimension: PdfBudgetDimension,
        limit: usize,
        actual: usize,
    },
}

/// Extract per-page text from PDF `bytes`, in page order (entry 0 is
/// page 1). A page with no text layer yields an empty string — that is
/// a scanned page, honest input for the OCR part of #171, not a
/// failure. Pure: takes bytes and a budget, no storage, no I/O.
pub fn extract_pdf_text(
    bytes: &[u8],
    budget: &PdfExtractionBudget,
) -> Result<Vec<String>, PdfTextError> {
    if bytes.len() > budget.max_bytes {
        return Err(PdfTextError::OverBudget {
            dimension: PdfBudgetDimension::Bytes,
            limit: budget.max_bytes,
            actual: bytes.len(),
        });
    }
    if !has_pdf_header(bytes) {
        return Err(PdfTextError::Unsupported);
    }
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| PdfTextError::Corrupt(e.to_string()))?;
    // lopdf loads an encrypted document even when the empty user
    // password does not authenticate; `was_encrypted` is set only when
    // it did. Extracting still-encrypted bytes would yield garbage, so
    // password-protected documents are reported explicitly. Documents
    // that decrypt with the empty user password (owner-password-locked
    // or no user password) extract normally.
    if doc.is_encrypted() && !doc.was_encrypted() {
        return Err(PdfTextError::Encrypted);
    }
    let page_count = doc.get_pages().len();
    if page_count == 0 {
        return Err(PdfTextError::Corrupt("no pages found".to_string()));
    }
    if page_count > budget.max_pages {
        return Err(PdfTextError::OverBudget {
            dimension: PdfBudgetDimension::Pages,
            limit: budget.max_pages,
            actual: page_count,
        });
    }
    let mut pages = Vec::with_capacity(page_count);
    for number in 1..=(page_count as u32) {
        let mut text = String::new();
        let mut output = pdf_extract::PlainTextOutput::new(&mut text);
        pdf_extract::output_doc_page(&doc, &mut output, number).map_err(map_extract_error)?;
        pages.push(text);
    }
    Ok(pages)
}

/// Map extraction-time failures onto the explicit variants: encryption
/// errors surface here for documents that reach decryption, and
/// everything else means the file is broken.
fn map_extract_error(err: OutputError) -> PdfTextError {
    match err {
        OutputError::PdfError(e) => match e {
            lopdf::Error::AlreadyEncrypted
            | lopdf::Error::Decryption(_)
            | lopdf::Error::InvalidPassword => PdfTextError::Encrypted,
            other => PdfTextError::Corrupt(other.to_string()),
        },
        other => PdfTextError::Corrupt(other.to_string()),
    }
}

/// PDFs must start with `%PDF-`; some writers prepend junk, and readers
/// accept the header anywhere in the first 1024 bytes.
fn has_pdf_header(bytes: &[u8]) -> bool {
    let scan = &bytes[..bytes.len().min(1024)];
    scan.windows(5).any(|window| window == b"%PDF-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real small files committed with the repo — the same fixtures
    /// tests/media_extract.rs imports as chat-export media.
    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(FIXTURES).join(name)).unwrap()
    }

    fn generous_budget() -> PdfExtractionBudget {
        PdfExtractionBudget::new(16 * 1024 * 1024, 512)
    }

    #[test]
    fn plan_pdf_yields_its_single_page_with_honest_empty_text() {
        // The committed plan.pdf is a minimal one-page PDF with no text
        // layer: extraction succeeds with one page whose text is empty —
        // the honest representation of a scanned/image-only page (OCR is
        // a later part of #171), not a failure.
        let pages = extract_pdf_text(&fixture("plan.pdf"), &generous_budget()).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].trim(), "");
    }

    #[test]
    fn unicode_pdf_decodes_non_ascii_text_per_page() {
        let pages = extract_pdf_text(&fixture("unicode_text.pdf"), &generous_budget()).unwrap();
        assert_eq!(pages.len(), 2, "two committed pages");
        assert!(pages[0].contains("Grüße"), "in {:?}", pages[0]);
        assert!(pages[0].contains("München"), "in {:?}", pages[0]);
        assert!(pages[0].contains("naïve café"), "in {:?}", pages[0]);
        assert!(pages[0].contains("25 °C"), "in {:?}", pages[0]);
        assert!(pages[0].contains("§4"), "in {:?}", pages[0]);
        assert!(pages[1].contains("Zoé"), "in {:?}", pages[1]);
        assert!(pages[1].contains("Ångström"), "in {:?}", pages[1]);
        assert!(pages[1].contains("¡Hola! ¿Qué tal?"), "in {:?}", pages[1]);
        assert!(pages[1].contains("Ærøskøbing"), "in {:?}", pages[1]);
    }

    #[test]
    fn password_protected_pdf_is_reported_as_encrypted() {
        let err = extract_pdf_text(&fixture("encrypted.pdf"), &generous_budget()).unwrap_err();
        assert_eq!(err, PdfTextError::Encrypted);
    }

    #[test]
    fn non_pdf_input_is_unsupported() {
        let budget = generous_budget();
        assert_eq!(
            extract_pdf_text(b"hello, plain text", &budget).unwrap_err(),
            PdfTextError::Unsupported
        );
        // A real non-PDF fixture: the committed JPEG.
        assert_eq!(
            extract_pdf_text(&fixture("photo.jpg"), &budget).unwrap_err(),
            PdfTextError::Unsupported
        );
        assert_eq!(
            extract_pdf_text(b"", &budget).unwrap_err(),
            PdfTextError::Unsupported
        );
    }

    #[test]
    fn corrupt_pdf_is_reported_explicitly() {
        let err =
            extract_pdf_text(b"%PDF-1.7\nbroken beyond repair", &generous_budget()).unwrap_err();
        assert!(matches!(err, PdfTextError::Corrupt(_)), "{err}");
    }

    #[test]
    fn oversized_input_exceeds_the_byte_budget() {
        let bytes = fixture("plan.pdf");
        let budget = PdfExtractionBudget::new(bytes.len() - 1, 512);
        assert_eq!(
            extract_pdf_text(&bytes, &budget).unwrap_err(),
            PdfTextError::OverBudget {
                dimension: PdfBudgetDimension::Bytes,
                limit: bytes.len() - 1,
                actual: bytes.len(),
            }
        );
    }

    #[test]
    fn page_count_exceeds_the_page_budget() {
        let bytes = fixture("unicode_text.pdf");
        let budget = PdfExtractionBudget::new(16 * 1024 * 1024, 1);
        assert_eq!(
            extract_pdf_text(&bytes, &budget).unwrap_err(),
            PdfTextError::OverBudget {
                dimension: PdfBudgetDimension::Pages,
                limit: 1,
                actual: 2,
            }
        );
    }
}
