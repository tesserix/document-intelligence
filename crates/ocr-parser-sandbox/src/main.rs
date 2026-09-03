use std::{
    env,
    ffi::OsString,
    io::{self, Read},
    process::ExitCode,
};

use ocr_parser_sandbox::{inspect_document, DocumentLimits, Error, MAXIMUM_ENCODED_BYTES};

fn main() -> ExitCode {
    let Some((content_type, limits)) = configuration(env::args_os().skip(1)) else {
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
    match inspect_document(&encoded, &content_type, limits) {
        Ok(report) => match serde_json::to_writer(io::stdout().lock(), &report) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(13),
        },
        Err(Error::UnsupportedContentType | Error::InvalidDocument) => ExitCode::from(10),
        Err(Error::PasswordProtected) => ExitCode::from(12),
        Err(
            Error::InvalidLimits
            | Error::EncodedByteLimitExceeded { .. }
            | Error::PageLimitExceeded { .. }
            | Error::PixelLimitExceeded { .. }
            | Error::ObjectLimitExceeded { .. }
            | Error::DecodedStreamLimitExceeded { .. }
            | Error::DecompressionRatioExceeded { .. },
        ) => ExitCode::from(11),
    }
}

fn configuration(arguments: impl Iterator<Item = OsString>) -> Option<(String, DocumentLimits)> {
    let arguments = arguments.collect::<Vec<_>>();
    if arguments.len() != 8 {
        return None;
    }
    let mut content_type = None;
    let mut maximum_pages = None;
    let mut maximum_page_pixels = None;
    let mut maximum_total_pixels = None;
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
            _ => return None,
        }
    }
    let limits =
        DocumentLimits::new(maximum_pages?, maximum_page_pixels?, maximum_total_pixels?).ok()?;
    Some((content_type?, limits))
}
