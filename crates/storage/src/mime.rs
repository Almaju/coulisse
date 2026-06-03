/// MIME types Coulisse accepts at upload. Rejects anything that isn't
/// in this list to prevent MIME-spoofing of executables into LLM backends.
/// Checked against inferred magic-bytes type, not just the declared
/// Content-Type.
pub const ALLOWED_MIME_PREFIXES: &[&str] = &[
    "application/json",
    "application/octet-stream",
    "application/pdf",
    "image/",
    "text/",
];

/// Check whether `mime_type` is in the allow-list.
#[must_use]
pub fn is_allowed(mime_type: &str) -> bool {
    ALLOWED_MIME_PREFIXES
        .iter()
        .any(|prefix| mime_type.starts_with(prefix))
}

/// Infer the MIME type from the first bytes of the file. Falls back to
/// `application/octet-stream` if inference fails (always in the allow-list).
#[must_use]
pub fn infer_mime(data: &[u8]) -> &'static str {
    infer::get(data).map_or("application/octet-stream", |t| t.mime_type())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_plain_is_allowed() {
        assert!(is_allowed("text/plain"));
    }

    #[test]
    fn image_png_is_allowed() {
        assert!(is_allowed("image/png"));
    }

    #[test]
    fn application_pdf_is_allowed() {
        assert!(is_allowed("application/pdf"));
    }

    #[test]
    fn application_octet_stream_is_allowed() {
        assert!(is_allowed("application/octet-stream"));
    }

    #[test]
    fn application_x_msdownload_is_rejected() {
        assert!(!is_allowed("application/x-msdownload"));
    }

    #[test]
    fn application_x_executable_is_rejected() {
        assert!(!is_allowed("application/x-executable"));
    }

    #[test]
    fn unknown_falls_back_to_octet_stream() {
        let data = b"\x00\x01\x02\x03\x04\x05\x06\x07";
        let mime = infer_mime(data);
        assert_eq!(mime, "application/octet-stream");
    }
}
