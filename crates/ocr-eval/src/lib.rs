//! Bounded, provider-neutral OCR evaluation metrics.

use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const HARD_MAXIMUM_UNITS: usize = 1_000_000;
const HARD_MAXIMUM_COMPARISON_CELLS: usize = 100_000_000;

pub const NORMALIZATION_POLICY_VERSION: &str = "unicode-nfc-whitespace-v1";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("text evaluation limits are invalid")]
    InvalidLimits,
    #[error("text evaluation input exceeds its unit limit")]
    InputLimitExceeded,
    #[error("text evaluation exceeds its comparison work limit")]
    ComparisonLimitExceeded,
    #[error("text evaluation aggregate exceeds its numeric limit")]
    AggregateOverflow,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TextEvaluationLimits {
    maximum_reference_units: usize,
    maximum_candidate_units: usize,
    maximum_comparison_cells: usize,
}

impl TextEvaluationLimits {
    pub fn new(
        maximum_reference_units: usize,
        maximum_candidate_units: usize,
        maximum_comparison_cells: usize,
    ) -> Result<Self> {
        if maximum_reference_units == 0
            || maximum_reference_units > HARD_MAXIMUM_UNITS
            || maximum_candidate_units == 0
            || maximum_candidate_units > HARD_MAXIMUM_UNITS
            || maximum_comparison_cells == 0
            || maximum_comparison_cells > HARD_MAXIMUM_COMPARISON_CELLS
        {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            maximum_reference_units,
            maximum_candidate_units,
            maximum_comparison_cells,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct ErrorRate {
    edits: usize,
    reference_units: usize,
    rate: Option<f64>,
}

impl ErrorRate {
    fn new(edits: usize, reference_units: usize, candidate_units: usize) -> Self {
        let rate = if reference_units == 0 {
            (candidate_units == 0).then_some(0.0)
        } else {
            Some(edits as f64 / reference_units as f64)
        };
        Self {
            edits,
            reference_units,
            rate,
        }
    }

    pub fn edits(self) -> usize {
        self.edits
    }

    pub fn reference_units(self) -> usize {
        self.reference_units
    }

    pub fn rate(self) -> Option<f64> {
        self.rate
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct TextEvaluation {
    characters: ErrorRate,
    words: ErrorRate,
}

impl TextEvaluation {
    pub fn policy_version(self) -> &'static str {
        NORMALIZATION_POLICY_VERSION
    }

    pub fn characters(self) -> ErrorRate {
        self.characters
    }

    pub fn words(self) -> ErrorRate {
        self.words
    }
}

pub fn evaluate_text(
    reference: &str,
    candidate: &str,
    limits: TextEvaluationLimits,
) -> Result<TextEvaluation> {
    ensure_unit_limits(
        bounded_character_count(reference, limits.maximum_reference_units),
        bounded_character_count(candidate, limits.maximum_candidate_units),
        limits,
    )?;
    let reference = reference.nfc().collect::<String>();
    let candidate = candidate.nfc().collect::<String>();
    let reference_characters = reference.chars().collect::<Vec<_>>();
    let candidate_characters = candidate.chars().collect::<Vec<_>>();
    ensure_unit_limits(
        reference_characters.len(),
        candidate_characters.len(),
        limits,
    )?;
    let reference_words = reference.split_whitespace().collect::<Vec<_>>();
    let candidate_words = candidate.split_whitespace().collect::<Vec<_>>();
    let character_cells = comparison_cells(reference_characters.len(), candidate_characters.len())?;
    let word_cells = comparison_cells(reference_words.len(), candidate_words.len())?;
    if character_cells
        .checked_add(word_cells)
        .is_none_or(|cells| cells > limits.maximum_comparison_cells)
    {
        return Err(Error::ComparisonLimitExceeded);
    }

    Ok(TextEvaluation {
        characters: ErrorRate::new(
            levenshtein(&reference_characters, &candidate_characters),
            reference_characters.len(),
            candidate_characters.len(),
        ),
        words: ErrorRate::new(
            levenshtein(&reference_words, &candidate_words),
            reference_words.len(),
            candidate_words.len(),
        ),
    })
}

fn bounded_character_count(value: &str, maximum: usize) -> usize {
    value.chars().take(maximum.saturating_add(1)).count()
}

pub fn aggregate_text_evaluations(evaluations: &[TextEvaluation]) -> Result<TextEvaluation> {
    let characters = aggregate_error_rates(evaluations.iter().map(|value| value.characters))?;
    let words = aggregate_error_rates(evaluations.iter().map(|value| value.words))?;
    Ok(TextEvaluation { characters, words })
}

fn aggregate_error_rates(mut rates: impl Iterator<Item = ErrorRate>) -> Result<ErrorRate> {
    let (edits, reference_units) = rates.try_fold((0_usize, 0_usize), |totals, rate| {
        Ok::<_, Error>((
            totals
                .0
                .checked_add(rate.edits)
                .ok_or(Error::AggregateOverflow)?,
            totals
                .1
                .checked_add(rate.reference_units)
                .ok_or(Error::AggregateOverflow)?,
        ))
    })?;
    Ok(ErrorRate::new(edits, reference_units, edits))
}

fn ensure_unit_limits(
    reference_units: usize,
    candidate_units: usize,
    limits: TextEvaluationLimits,
) -> Result<()> {
    if reference_units > limits.maximum_reference_units
        || candidate_units > limits.maximum_candidate_units
    {
        Err(Error::InputLimitExceeded)
    } else {
        Ok(())
    }
}

fn comparison_cells(reference_units: usize, candidate_units: usize) -> Result<usize> {
    reference_units
        .checked_mul(candidate_units)
        .ok_or(Error::ComparisonLimitExceeded)
}

fn levenshtein<T: Eq>(left: &[T], right: &[T]) -> usize {
    let (rows, columns) = if left.len() >= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let mut previous = (0..=columns.len()).collect::<Vec<_>>();
    let mut current = vec![0; columns.len() + 1];
    for (row_index, row) in rows.iter().enumerate() {
        current[0] = row_index + 1;
        for (column_index, column) in columns.iter().enumerate() {
            current[column_index + 1] = (current[column_index] + 1)
                .min(previous[column_index + 1] + 1)
                .min(previous[column_index] + usize::from(row != column));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[columns.len()]
}
