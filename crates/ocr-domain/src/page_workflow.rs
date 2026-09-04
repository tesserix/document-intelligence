use serde::{Deserialize, Serialize};

use crate::{Error, JobId, Result};

pub const MAXIMUM_PAGE_COUNT: u32 = 300;
pub const MAXIMUM_PAGE_ATTEMPTS: u8 = 10;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PageWorkflowStatus {
    Running,
    Completed,
    Partial,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTask {
    pub page: u32,
    pub attempt: u8,
    pub activity_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum PageProgress {
    Pending { failures: u8 },
    Running { attempt: u8 },
    Succeeded { attempt: u8 },
    Exhausted { attempts: u8 },
    PermanentlyFailed { attempt: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPageWorkflow")]
pub struct PageWorkflow {
    job_id: JobId,
    max_attempts: u8,
    pages: Vec<PageProgress>,
    cancelled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPageWorkflow {
    job_id: JobId,
    max_attempts: u8,
    pages: Vec<PageProgress>,
    #[serde(default)]
    cancelled: bool,
}

impl TryFrom<RawPageWorkflow> for PageWorkflow {
    type Error = Error;

    fn try_from(value: RawPageWorkflow) -> Result<Self> {
        if !(1..=MAXIMUM_PAGE_ATTEMPTS).contains(&value.max_attempts)
            || value.pages.is_empty()
            || value.pages.len() > MAXIMUM_PAGE_COUNT as usize
            || value.pages.iter().any(|page| match page {
                PageProgress::Pending { failures } => *failures >= value.max_attempts,
                PageProgress::Running { attempt } => *attempt == 0 || *attempt > value.max_attempts,
                PageProgress::Succeeded { attempt } => {
                    *attempt == 0 || *attempt > value.max_attempts
                }
                PageProgress::Exhausted { attempts } => *attempts != value.max_attempts,
                PageProgress::PermanentlyFailed { attempt } => {
                    *attempt == 0 || *attempt > value.max_attempts
                }
            })
        {
            return Err(Error::InvalidPageWorkflow);
        }
        Ok(Self {
            job_id: value.job_id,
            max_attempts: value.max_attempts,
            pages: value.pages,
            cancelled: value.cancelled,
        })
    }
}

impl PageWorkflow {
    pub fn new(job_id: JobId, page_count: u32, max_attempts: u8) -> Result<Self> {
        if !(1..=MAXIMUM_PAGE_COUNT).contains(&page_count)
            || !(1..=MAXIMUM_PAGE_ATTEMPTS).contains(&max_attempts)
        {
            return Err(Error::InvalidPageWorkflow);
        }
        let page_count = usize::try_from(page_count).map_err(|_| Error::InvalidPageWorkflow)?;
        Ok(Self {
            job_id,
            max_attempts,
            pages: vec![PageProgress::Pending { failures: 0 }; page_count],
            cancelled: false,
        })
    }

    pub fn status(&self) -> PageWorkflowStatus {
        if self.cancelled {
            PageWorkflowStatus::Cancelled
        } else if self
            .pages
            .iter()
            .all(|page| matches!(page, PageProgress::Succeeded { .. }))
        {
            PageWorkflowStatus::Completed
        } else if self.pages.iter().all(|page| {
            matches!(
                page,
                PageProgress::Succeeded { .. }
                    | PageProgress::Exhausted { .. }
                    | PageProgress::PermanentlyFailed { .. }
            )
        }) {
            PageWorkflowStatus::Partial
        } else {
            PageWorkflowStatus::Running
        }
    }

    pub fn job_id(&self) -> &JobId {
        &self.job_id
    }

    pub fn successful_page_count(&self) -> usize {
        self.pages
            .iter()
            .filter(|page| matches!(page, PageProgress::Succeeded { .. }))
            .count()
    }

    pub fn claim_ready(&mut self, limit: usize) -> Result<Vec<PageTask>> {
        if !(1..=64).contains(&limit) {
            return Err(Error::InvalidPageWorkflow);
        }
        if self.cancelled {
            return Ok(Vec::new());
        }
        let mut tasks = Vec::with_capacity(limit.min(self.pages.len()));
        let job_id = self.job_id.as_str().to_owned();
        for (index, progress) in self.pages.iter_mut().enumerate() {
            if tasks.len() == limit {
                break;
            }
            let attempt = match progress {
                PageProgress::Running { attempt } => *attempt,
                PageProgress::Pending { failures } => {
                    let attempt = failures.saturating_add(1);
                    *progress = PageProgress::Running { attempt };
                    attempt
                }
                PageProgress::Succeeded { .. }
                | PageProgress::Exhausted { .. }
                | PageProgress::PermanentlyFailed { .. } => continue,
            };
            let page = u32::try_from(index + 1).map_err(|_| Error::InvalidPageWorkflow)?;
            tasks.push(PageTask {
                page,
                attempt,
                activity_key: format!("ocr-job-{job_id}-page-{page}-attempt-{attempt}"),
            });
        }
        Ok(tasks)
    }

    pub fn record_success(&mut self, task: &PageTask) -> Result<()> {
        let progress = self.active_progress(task)?;
        *progress = PageProgress::Succeeded {
            attempt: task.attempt,
        };
        Ok(())
    }

    pub fn is_successful_task(&self, task: &PageTask) -> bool {
        if task.page == 0 || task.activity_key != self.activity_key(task.page, task.attempt) {
            return false;
        }
        usize::try_from(task.page - 1)
            .ok()
            .and_then(|index| self.pages.get(index))
            .is_some_and(
                |progress| matches!(progress, PageProgress::Succeeded { attempt } if *attempt == task.attempt),
            )
    }

    pub fn record_retryable_failure(&mut self, task: &PageTask) -> Result<()> {
        let max_attempts = self.max_attempts;
        let progress = self.active_progress(task)?;
        *progress = if task.attempt == max_attempts {
            PageProgress::Exhausted {
                attempts: task.attempt,
            }
        } else {
            PageProgress::Pending {
                failures: task.attempt,
            }
        };
        Ok(())
    }

    pub fn record_permanent_failure(&mut self, task: &PageTask) -> Result<()> {
        let progress = self.active_progress(task)?;
        *progress = PageProgress::PermanentlyFailed {
            attempt: task.attempt,
        };
        Ok(())
    }

    pub fn request_cancellation(&mut self) {
        self.cancelled = true;
    }

    fn active_progress(&mut self, task: &PageTask) -> Result<&mut PageProgress> {
        if self.cancelled
            || task.activity_key != self.activity_key(task.page, task.attempt)
            || task.page == 0
        {
            return Err(Error::StalePageTask);
        }
        let index = usize::try_from(task.page - 1).map_err(|_| Error::StalePageTask)?;
        let progress = self.pages.get_mut(index).ok_or(Error::StalePageTask)?;
        if !matches!(progress, PageProgress::Running { attempt } if *attempt == task.attempt) {
            return Err(Error::StalePageTask);
        }
        Ok(progress)
    }

    fn activity_key(&self, page: u32, attempt: u8) -> String {
        format!(
            "ocr-job-{}-page-{page}-attempt-{attempt}",
            self.job_id.as_str()
        )
    }
}
