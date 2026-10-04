use crate::{evaluate_text, TextEvaluationLimits};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum BenchmarkError {
    #[error("benchmark inputs are invalid, incomplete, or mismatched")]
    Invalid,
    #[error(transparent)]
    Evaluation(#[from] crate::Error),
}

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    sha256: String,
    reference_text: Option<String>,
    expected_fields: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
struct Sample {
    id: String,
    sha256: String,
    status: String,
    elapsed_ms: u64,
    text: String,
    fields: BTreeMap<String, Value>,
}
#[derive(Debug, Serialize)]
pub struct CaseScore {
    pub id: String,
    pub status: String,
    pub character_error_rate: Option<f64>,
    pub word_error_rate: Option<f64>,
    pub expected_fields: usize,
    pub correct_fields: usize,
    pub missing_fields: usize,
    pub wrong_fields: usize,
    pub elapsed_ms: u64,
}
#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub normalization_policy: &'static str,
    pub cases: Vec<CaseScore>,
    pub p50_ms: u64,
    pub p95_ms: u64,
}

pub fn score_benchmark(manifest: &str, samples: &str) -> Result<BenchmarkReport, BenchmarkError> {
    if manifest.len() > 16 * 1024 * 1024 || samples.len() > 16 * 1024 * 1024 {
        return Err(BenchmarkError::Invalid);
    }
    let manifest: Manifest = serde_json::from_str(manifest).map_err(|_| BenchmarkError::Invalid)?;
    let samples: Vec<Sample> =
        serde_json::from_str(samples).map_err(|_| BenchmarkError::Invalid)?;
    if manifest.cases.is_empty()
        || manifest.cases.len() > 1000
        || samples.len() != manifest.cases.len()
    {
        return Err(BenchmarkError::Invalid);
    }
    let mut seen = BTreeSet::new();
    let mut by_id = BTreeMap::new();
    for sample in samples {
        if !matches!(
            sample.status.as_str(),
            "completed" | "partial" | "review_required" | "failed" | "cancelled"
        ) || by_id.insert(sample.id.clone(), sample).is_some()
        {
            return Err(BenchmarkError::Invalid);
        }
    }
    let mut scores = Vec::new();
    for case in manifest.cases {
        if case.id.is_empty() || case.id.len() > 128 || !seen.insert(case.id.clone()) {
            return Err(BenchmarkError::Invalid);
        }
        let sample = by_id.get(&case.id).ok_or(BenchmarkError::Invalid)?;
        if sample.sha256 != case.sha256 {
            return Err(BenchmarkError::Invalid);
        }
        let evaluation = case
            .reference_text
            .as_ref()
            .map(|text| {
                evaluate_text(
                    text,
                    &sample.text,
                    TextEvaluationLimits::new(100_000, 100_000, 10_000_000)?,
                )
            })
            .transpose()?;
        let mut correct = 0;
        let mut missing = 0;
        let mut wrong = 0;
        for (key, expected) in &case.expected_fields {
            match sample.fields.get(key).and_then(|field| field.get("value")) {
                None => missing += 1,
                Some(value) => {
                    let matches = equivalent(expected, value);
                    if matches {
                        correct += 1;
                    } else {
                        wrong += 1;
                    }
                }
            }
        }
        scores.push(CaseScore {
            id: case.id,
            status: sample.status.clone(),
            character_error_rate: evaluation.and_then(|e| e.characters().rate()),
            word_error_rate: evaluation.and_then(|e| e.words().rate()),
            expected_fields: case.expected_fields.len(),
            correct_fields: correct,
            missing_fields: missing,
            wrong_fields: wrong,
            elapsed_ms: sample.elapsed_ms,
        });
    }
    let mut times = scores.iter().map(|s| s.elapsed_ms).collect::<Vec<_>>();
    times.sort_unstable();
    Ok(BenchmarkReport {
        normalization_policy: crate::NORMALIZATION_POLICY_VERSION,
        p50_ms: times[(times.len() * 50).div_ceil(100) - 1],
        p95_ms: times[(times.len() * 95).div_ceil(100) - 1],
        cases: scores,
    })
}

fn equivalent(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => (a - b).abs() <= 0.01,
            _ => a == b,
        },
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| equivalent(value, other)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equivalent(a, b))
        }
        _ => expected == actual,
    }
}
