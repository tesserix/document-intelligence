use std::{
    env,
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    process::ExitCode,
};

use ocr_parser_sandbox::{inspect_document, DocumentLimits, Error, MAXIMUM_ENCODED_BYTES};

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1).peekable();
    let prepare = arguments.peek().is_some_and(|value| value == "prepare-ocr");
    let native = arguments
        .peek()
        .is_some_and(|value| value == "extract-native");
    if prepare || native {
        arguments.next();
    }
    let Some((content_type, limits, pdf_text_executable)) = configuration(arguments) else {
        return ExitCode::from(2);
    };
    let maximum_read = match u64::try_from(MAXIMUM_ENCODED_BYTES)
        .ok()
        .and_then(|value| value.checked_add(1))
    {
        Some(value) => value,
        None => return ExitCode::from(13),
    };
    let mut encoded = Vec::new();
    if io::stdin()
        .lock()
        .take(maximum_read)
        .read_to_end(&mut encoded)
        .is_err()
    {
        return ExitCode::from(13);
    }
    if native {
        return match ocr_parser_sandbox::extract_native_document(
            &encoded,
            &content_type,
            limits,
            &pdf_text_executable,
        ) {
            Ok(pages) => match serde_json::to_vec(&pages) {
                Ok(bytes) if bytes.len() <= ocr_domain::MAXIMUM_DOCUMENT_RESULT_BYTES => {
                    if io::stdout().lock().write_all(&bytes).is_ok() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(13)
                    }
                }
                Ok(_) => ExitCode::from(11),
                Err(_) => ExitCode::from(13),
            },
            Err(error) => error_exit(error),
        };
    }
    if prepare {
        return match ocr_parser_sandbox::prepare_ocr_image(&encoded, &content_type) {
            Ok(Some(bytes)) => {
                if io::stdout().lock().write_all(&bytes).is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(13)
                }
            }
            Ok(None) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(10),
        };
    }
    match inspect_document(&encoded, &content_type, limits) {
        Ok(report) => match serde_json::to_writer(io::stdout().lock(), &report) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(13),
        },
        Err(error) => error_exit(error),
    }
}

fn configuration(
    arguments: impl Iterator<Item = OsString>,
) -> Option<(String, DocumentLimits, PathBuf)> {
    let arguments = arguments.collect::<Vec<_>>();
    if !matches!(arguments.len(), 8 | 10) {
        return None;
    }
    let mut content_type = None;
    let mut maximum_pages = None;
    let mut maximum_page_pixels = None;
    let mut maximum_total_pixels = None;
    let mut pdf_text_executable = None;
    for pair in arguments.chunks_exact(2) {
        let key = pair[0].to_str()?;
        let value = pair[1].to_str()?;
        match key {
            "--content-type" if content_type.is_none() => content_type = Some(value.to_owned()),
            "--max-pages" if maximum_pages.is_none() => maximum_pages = value.parse().ok(),
            "--max-page-pixels" if maximum_page_pixels.is_none() => {
                maximum_page_pixels = value.parse().ok()
            }
            "--max-total-pixels" if maximum_total_pixels.is_none() => {
                maximum_total_pixels = value.parse().ok()
            }
            "--pdf-text-executable" if pdf_text_executable.is_none() => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return None;
                }
                pdf_text_executable = Some(path);
            }
            _ => return None,
        }
    }
    let limits =
        DocumentLimits::new(maximum_pages?, maximum_page_pixels?, maximum_total_pixels?).ok()?;
    Some((
        content_type?,
        limits,
        pdf_text_executable.unwrap_or_else(|| "/usr/bin/pdftotext".into()),
    ))
}

fn error_exit(error: Error) -> ExitCode {
    match error {
        Error::NativeTextUnavailable => ExitCode::from(13),
        Error::UnsupportedContentType | Error::InvalidDocument => ExitCode::from(10),
        Error::PasswordProtected => ExitCode::from(12),
        _ => ExitCode::from(11),
    }
}
