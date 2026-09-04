use std::{path::PathBuf, process::Stdio, time::Duration};

use ocr_domain::PageGeometry;
use serde::Deserialize;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

#[cfg(unix)]
#[derive(Debug)]
struct ParserProcessGroup {
    identifier: i32,
    active: bool,
}

#[cfg(unix)]
impl ParserProcessGroup {
    fn new(process_identifier: u32) -> Result<Self, ParserProcessError> {
        let identifier =
            i32::try_from(process_identifier).map_err(|_| ParserProcessError::Unavailable)?;
        if identifier <= 0 {
            return Err(ParserProcessError::Unavailable);
        }
        Ok(Self {
            identifier,
            active: true,
        })
    }

    fn terminate(&mut self) {
        if self.active {
            // The parser is isolated in this group, so descendants cannot outlive cancellation.
            let _ = unsafe { libc::kill(-self.identifier, libc::SIGKILL) };
            self.active = false;
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

#[cfg(unix)]
impl Drop for ParserProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

const MAXIMUM_INPUT_BYTES: usize = 100 * 1024 * 1024;
const MAXIMUM_OUTPUT_BYTES: usize = 64 * 1024;
pub const PARSER_PROFILE: &str = "intake-v1";
pub const PARSER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserInspectionReport {
    pub page_count: i32,
    pub maximum_page_pixels: i64,
    pub total_page_pixels: i64,
    pub pages: Vec<PageGeometry>,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum ParserProcessError {
    #[error("parser configuration is invalid")]
    InvalidConfiguration,
    #[error("document is invalid")]
    InvalidDocument,
    #[error("document exceeds parser limits")]
    LimitsExceeded,
    #[error("document password is required")]
    PasswordRequired,
    #[error("parser is unavailable")]
    Unavailable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReport {
    page_count: u64,
    maximum_page_pixels: u64,
    total_page_pixels: u64,
    pages: Vec<PageGeometry>,
    password_protected: bool,
}

#[derive(Debug, Clone)]
pub struct ParserProcess {
    executable: PathBuf,
    deadline: Duration,
}

impl ParserProcess {
    pub fn new(executable: PathBuf, deadline: Duration) -> Result<Self, ParserProcessError> {
        if !executable.is_absolute() || deadline.is_zero() || deadline > Duration::from_secs(120) {
            return Err(ParserProcessError::InvalidConfiguration);
        }
        Ok(Self {
            executable,
            deadline,
        })
    }

    pub async fn inspect(
        &self,
        encoded: &[u8],
        content_type: &str,
    ) -> Result<ParserInspectionReport, ParserProcessError> {
        if encoded.is_empty()
            || encoded.len() > MAXIMUM_INPUT_BYTES
            || !matches!(
                content_type,
                "application/pdf" | "image/jpeg" | "image/png" | "image/tiff" | "image/webp"
            )
        {
            return Err(ParserProcessError::InvalidDocument);
        }

        let mut command = Command::new(&self.executable);
        command
            .args([
                "--content-type",
                content_type,
                "--max-pages",
                "300",
                "--max-page-pixels",
                "100000000",
                "--max-total-pixels",
                "1000000000",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| ParserProcessError::Unavailable)?;
        #[cfg(unix)]
        let mut process_group =
            ParserProcessGroup::new(child.id().ok_or(ParserProcessError::Unavailable)?)?;
        let mut stdin = child.stdin.take().ok_or(ParserProcessError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(ParserProcessError::Unavailable)?;

        let operation = async {
            let write = async move {
                stdin
                    .write_all(encoded)
                    .await
                    .map_err(|_| ParserProcessError::Unavailable)?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|_| ParserProcessError::Unavailable)
            };
            let read = async move {
                let mut output = Vec::new();
                stdout
                    .take((MAXIMUM_OUTPUT_BYTES + 1) as u64)
                    .read_to_end(&mut output)
                    .await
                    .map_err(|_| ParserProcessError::Unavailable)?;
                if output.len() > MAXIMUM_OUTPUT_BYTES {
                    return Err(ParserProcessError::Unavailable);
                }
                Ok(output)
            };
            let wait = async {
                child
                    .wait()
                    .await
                    .map_err(|_| ParserProcessError::Unavailable)
            };
            tokio::try_join!(write, read, wait)
        };

        let (_, output, status) = match timeout(self.deadline, operation).await {
            Ok(Ok(result)) => {
                #[cfg(unix)]
                process_group.disarm();
                result
            }
            Ok(Err(error)) => {
                #[cfg(unix)]
                process_group.terminate();
                #[cfg(not(unix))]
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
            Err(_) => {
                #[cfg(unix)]
                process_group.terminate();
                #[cfg(not(unix))]
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ParserProcessError::Unavailable);
            }
        };
        match status.code() {
            Some(0) => parse_report(&output),
            Some(10) => Err(ParserProcessError::InvalidDocument),
            Some(11) => Err(ParserProcessError::LimitsExceeded),
            Some(12) => Err(ParserProcessError::PasswordRequired),
            _ => Err(ParserProcessError::Unavailable),
        }
    }
}

fn parse_report(output: &[u8]) -> Result<ParserInspectionReport, ParserProcessError> {
    let report: WireReport =
        serde_json::from_slice(output).map_err(|_| ParserProcessError::Unavailable)?;
    let page_count =
        i32::try_from(report.page_count).map_err(|_| ParserProcessError::Unavailable)?;
    let maximum_page_pixels =
        i64::try_from(report.maximum_page_pixels).map_err(|_| ParserProcessError::Unavailable)?;
    let total_page_pixels =
        i64::try_from(report.total_page_pixels).map_err(|_| ParserProcessError::Unavailable)?;
    if report.password_protected
        || !(1..=300).contains(&page_count)
        || !(1..=100_000_000).contains(&maximum_page_pixels)
        || !(maximum_page_pixels..=1_000_000_000).contains(&total_page_pixels)
        || report.pages.len()
            != usize::try_from(page_count).map_err(|_| ParserProcessError::Unavailable)?
    {
        return Err(ParserProcessError::Unavailable);
    }
    let mut computed_maximum = 0_i64;
    let mut computed_total = 0_i64;
    for (index, page) in report.pages.iter().enumerate() {
        let expected_page =
            u32::try_from(index + 1).map_err(|_| ParserProcessError::Unavailable)?;
        if u32::from(page.page) != expected_page {
            return Err(ParserProcessError::Unavailable);
        }
        let pixels = i64::try_from(page.pixels()).map_err(|_| ParserProcessError::Unavailable)?;
        if pixels > maximum_page_pixels {
            return Err(ParserProcessError::Unavailable);
        }
        computed_maximum = computed_maximum.max(pixels);
        computed_total = computed_total
            .checked_add(pixels)
            .ok_or(ParserProcessError::Unavailable)?;
    }
    if computed_maximum != maximum_page_pixels || computed_total != total_page_pixels {
        return Err(ParserProcessError::Unavailable);
    }
    Ok(ParserInspectionReport {
        page_count,
        maximum_page_pixels,
        total_page_pixels,
        pages: report.pages,
    })
}
