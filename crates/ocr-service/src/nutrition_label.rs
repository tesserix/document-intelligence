use std::collections::BTreeMap;

use ocr_domain::{
    DocumentPage, Evidence, ExtractedValue, ObservationLevel, PageNumber, StableCode,
    TextObservation, ValidationFailure, ValidationSeverity,
};
use serde_json::{json, Value};

use crate::extraction::ExtractedFields;

const KJ_PER_KCAL: f64 = 4.184;
const MAXIMUM_LABEL_CHARS: usize = 40;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Column {
    PerServing,
    Per100g,
    Per100ml,
}

impl Column {
    fn key(self) -> &'static str {
        match self {
            Self::PerServing => "per_serving",
            Self::Per100g => "per_100g",
            Self::Per100ml => "per_100ml",
        }
    }
}

const HEADERS: [(&str, Column); 6] = [
    ("per serving", Column::PerServing),
    ("per serve", Column::PerServing),
    ("per 100g", Column::Per100g),
    ("per 100 g", Column::Per100g),
    ("per 100ml", Column::Per100ml),
    ("per 100 ml", Column::Per100ml),
];

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Nutrient {
    EnergyKj,
    EnergyKcal,
    Protein,
    Fat,
    SaturatedFat,
    Carbohydrate,
    Sugars,
    Fibre,
    Sodium,
}

impl Nutrient {
    fn key(self) -> &'static str {
        match self {
            Self::EnergyKj => "energy_kj",
            Self::EnergyKcal => "energy_kcal",
            Self::Protein => "protein_g",
            Self::Fat => "fat_g",
            Self::SaturatedFat => "saturated_fat_g",
            Self::Carbohydrate => "carbohydrate_g",
            Self::Sugars => "sugars_g",
            Self::Fibre => "fibre_g",
            Self::Sodium => "sodium_mg",
        }
    }

    fn normalise_mass(self, amount: f64, unit: Unit) -> Option<f64> {
        let grams = match unit {
            Unit::G => amount,
            Unit::Mg => amount / 1_000.0,
            Unit::Mcg => amount / 1_000_000.0,
            _ => return None,
        };
        Some(if self == Self::Sodium {
            grams * 1_000.0
        } else {
            grams
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Unit {
    Kj,
    Kcal,
    G,
    Mg,
    Mcg,
    Ml,
    Percent,
    Other,
}

#[derive(Debug, Copy, Clone)]
struct Quantity {
    amount: f64,
    unit: Option<Unit>,
    upper_bound: bool,
}

#[derive(Debug, Copy, Clone)]
enum Row {
    Energy { calories: bool },
    Mass(Nutrient),
}

struct Line<'a> {
    page: PageNumber,
    observation: &'a TextObservation,
    lower: String,
}

struct Reading<'a> {
    nutrient: Nutrient,
    slot: usize,
    amount: f64,
    lines: Vec<&'a Line<'a>>,
}

#[derive(Default)]
struct Output {
    fields: BTreeMap<String, ExtractedValue>,
    validation_failures: Vec<ValidationFailure>,
}

impl Output {
    fn put(&mut self, name: &str, value: Value, lines: &[&Line]) {
        if self.fields.contains_key(name) {
            return;
        }
        let Some(confidence) = lines
            .iter()
            .map(|line| line.observation.confidence)
            .min_by(|a, b| f64::from(*a).total_cmp(&f64::from(*b)))
        else {
            return;
        };
        let evidence = lines
            .iter()
            .map(|line| {
                Evidence::new(
                    line.page,
                    line.observation.polygon.clone(),
                    line.observation.observation_id.clone(),
                )
            })
            .collect();
        if let Ok(extracted) = ExtractedValue::new(value, confidence, evidence) {
            self.fields.insert(name.to_owned(), extracted);
        }
    }

    fn fail(&mut self, code: &str, severity: ValidationSeverity) {
        let Ok(code) = StableCode::new(code) else {
            return;
        };
        if !self.validation_failures.iter().any(|f| f.code == code) {
            self.validation_failures
                .push(ValidationFailure::new(code, severity));
        }
    }
}

pub(crate) fn extract(pages: &[DocumentPage]) -> ExtractedFields {
    let lines = pages
        .iter()
        .flat_map(|page| {
            page.observations
                .iter()
                .filter(|observation| observation.level == ObservationLevel::Line)
                .map(|observation| Line {
                    page: page.page,
                    observation,
                    lower: observation.text.to_lowercase(),
                })
        })
        .collect::<Vec<_>>();

    let mut output = Output::default();
    let mut columns = Vec::<Column>::new();
    let mut header_lines = Vec::<&Line>::new();
    let mut serving = None::<(f64, Unit)>;
    let mut readings = Vec::<Reading>::new();
    let mut panel_rows = 0;

    for line in &lines {
        let text = line.lower.as_str();
        if text.contains("servings per") {
            if let Some(quantity) = quantities(text).first() {
                output.put("servings_per_pack", json!(round(quantity.amount)), &[line]);
            }
            continue;
        }
        let headers = column_headers(text);
        if !headers.is_empty() {
            for column in headers {
                if !columns.contains(&column) {
                    columns.push(column);
                }
            }
            header_lines.push(line);
            if let Some(size) = header_serving(text) {
                record_serving(&mut output, &mut serving, size, line);
            }
            continue;
        }
        if text.contains("serving size") || text.contains("serve size") {
            if let Some(size) = metric_quantity(text) {
                record_serving(&mut output, &mut serving, size, line);
            }
            continue;
        }
        if let Some((row, label_unit)) = classify(text) {
            let before = readings.len();
            read_row(row, label_unit, line, &mut readings);
            if readings.len() == before && quantities(text).is_empty() {
                read_spatial_row(row, label_unit, line, &lines, &columns, &mut readings);
                if !columns.is_empty()
                    && (0..columns.len()).any(|slot| {
                        !readings[before..]
                            .iter()
                            .any(|reading| reading.slot == slot)
                    })
                {
                    output.fail("nutrition_row_incomplete", ValidationSeverity::Error);
                }
            }
            if readings.len() > before || quantities(text).iter().any(|q| q.upper_bound) {
                panel_rows += 1;
            }
            continue;
        }
        if let Some(barcode) = barcode(&line.observation.text) {
            output.put("barcode", json!(barcode), &[line]);
        }
    }

    if let Some((locale, line)) = locale(&lines) {
        output.put("locale", json!(locale), &[line]);
    }

    if panel_rows == 0 {
        output.fail("nutrition_panel_not_found", ValidationSeverity::Error);
    } else if columns.is_empty() {
        output.fail("nutrition_columns_unidentified", ValidationSeverity::Error);
    } else {
        output.put(
            "column_headers",
            json!(columns.iter().map(|c| c.key()).collect::<Vec<_>>()),
            &header_lines,
        );
        let mut values = BTreeMap::new();
        for reading in readings {
            let Some(column) = columns.get(reading.slot).copied() else {
                continue;
            };
            let name = format!("{}.{}", column.key(), reading.nutrient.key());
            if values.contains_key(&(column, reading.nutrient)) {
                continue;
            }
            values.insert((column, reading.nutrient), reading.amount);
            output.put(&name, json!(round(reading.amount)), &reading.lines);
        }
        validate(&mut output, &columns, &values, serving);
    }

    ExtractedFields {
        fields: output.fields,
        validation_failures: output.validation_failures,
    }
}

fn record_serving(
    output: &mut Output,
    serving: &mut Option<(f64, Unit)>,
    size: Quantity,
    line: &Line,
) {
    let Some(unit) = size.unit else { return };
    if serving.is_some() {
        return;
    }
    *serving = Some((size.amount, unit));
    let unit = if unit == Unit::Ml { "ml" } else { "g" };
    output.put(
        "serving_size",
        json!({"amount": round(size.amount), "unit": unit}),
        &[line],
    );
}

fn column_headers(text: &str) -> Vec<Column> {
    let mut found = HEADERS
        .iter()
        .flat_map(|(phrase, column)| text.match_indices(phrase).map(|(at, _)| (at, *column)))
        .collect::<Vec<_>>();
    found.sort();
    let mut columns = Vec::new();
    for (_, column) in found {
        if !columns.contains(&column) {
            columns.push(column);
        }
    }
    columns
}

fn classify(text: &str) -> Option<(Row, Option<Unit>)> {
    let label = &text[..text
        .find(|c: char| c.is_ascii_digit() || c == '<')
        .unwrap_or(text.len())];
    if label.chars().count() > MAXIMUM_LABEL_CHARS {
        return None;
    }
    let label = label.trim_start_matches(|c: char| "-–—•*· ".contains(c));
    let label = label.strip_prefix("of which ").unwrap_or(label);
    let label = label.strip_prefix("total ").unwrap_or(label);
    if ["unsaturated", "trans", "added", "includes", "from"]
        .iter()
        .any(|word| label.contains(word))
    {
        return None;
    }
    let row = if label.starts_with("saturated") {
        Row::Mass(Nutrient::SaturatedFat)
    } else if label.starts_with("sugar") {
        Row::Mass(Nutrient::Sugars)
    } else if ["fibre", "fiber", "dietary fibre", "dietary fiber"]
        .iter()
        .any(|prefix| label.starts_with(prefix))
    {
        Row::Mass(Nutrient::Fibre)
    } else if label.starts_with("energy") {
        Row::Energy { calories: false }
    } else if label.starts_with("calories") {
        Row::Energy { calories: true }
    } else if label.starts_with("protein") {
        Row::Mass(Nutrient::Protein)
    } else if label.starts_with("fat") {
        Row::Mass(Nutrient::Fat)
    } else if label.starts_with("carbohydrate") {
        Row::Mass(Nutrient::Carbohydrate)
    } else if label.starts_with("sodium") {
        Row::Mass(Nutrient::Sodium)
    } else {
        return None;
    };
    let label_unit = [
        ("(g)", Unit::G),
        ("(mg)", Unit::Mg),
        ("(kcal)", Unit::Kcal),
        ("(kj)", Unit::Kj),
    ]
    .into_iter()
    .find_map(|(marker, unit)| label.contains(marker).then_some(unit));
    Some((row, label_unit))
}

// Each unit keeps its own slot order, so "540kJ (129Cal) 1800kJ (430Cal)" maps kJ and kcal to the same columns.
fn read_row<'a>(
    row: Row,
    label_unit: Option<Unit>,
    line: &'a Line<'a>,
    readings: &mut Vec<Reading<'a>>,
) {
    let mut slots = BTreeMap::<Nutrient, usize>::new();
    for quantity in quantities(&line.lower) {
        let unit = quantity.unit.or(label_unit).or(match row {
            Row::Energy { calories: true } => Some(Unit::Kcal),
            _ => None,
        });
        let reading = match (row, unit) {
            (Row::Energy { .. }, Some(Unit::Kj)) => Some((Nutrient::EnergyKj, quantity.amount)),
            (Row::Energy { .. }, Some(Unit::Kcal)) => Some((Nutrient::EnergyKcal, quantity.amount)),
            (Row::Mass(nutrient), Some(unit)) => nutrient
                .normalise_mass(quantity.amount, unit)
                .map(|amount| (nutrient, amount)),
            _ => None,
        };
        let Some((nutrient, amount)) = reading else {
            continue;
        };
        let slot = slots.entry(nutrient).or_default();
        if !quantity.upper_bound {
            readings.push(Reading {
                nutrient,
                slot: *slot,
                amount,
                lines: vec![line],
            });
        }
        *slot += 1;
    }
}

// Project onto the panel's text direction, including quarter-turn and tilted photos.
fn panel_axis(lines: &[Line<'_>], page: PageNumber) -> (f64, f64) {
    let mut angles = lines
        .iter()
        .filter(|line| line.page == page && classify(&line.lower).is_some())
        .filter_map(|line| {
            let points = &line.observation.polygon.points;
            let a = points.first()?;
            let b = points.get(1)?;
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            (dx.hypot(dy) > f64::EPSILON).then(|| dy.atan2(dx))
        })
        .collect::<Vec<_>>();
    angles.sort_by(f64::total_cmp);
    let angle = angles.get(angles.len() / 2).copied().unwrap_or(0.0);
    (angle.cos(), angle.sin())
}

fn bounds(line: &Line<'_>, axis: (f64, f64)) -> [f64; 4] {
    let mut bounds = [
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    for point in &line.observation.polygon.points {
        let x = point.x * axis.0 + point.y * axis.1;
        let y = -point.x * axis.1 + point.y * axis.0;
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].max(x);
        bounds[2] = bounds[2].min(y);
        bounds[3] = bounds[3].max(y);
    }
    bounds
}

fn aligned_row(a: [f64; 4], b: [f64; 4]) -> bool {
    let overlap = a[3].min(b[3]) - a[2].max(b[2]);
    overlap > 0.5 * (a[3] - a[2]).min(b[3] - b[2])
}

fn read_spatial_row<'a>(
    row: Row,
    label_unit: Option<Unit>,
    label: &'a Line<'a>,
    lines: &'a [Line<'a>],
    columns: &[Column],
    readings: &mut Vec<Reading<'a>>,
) {
    let axis = panel_axis(lines, label.page);
    let label_bounds = bounds(label, axis);
    let headers = lines
        .iter()
        .filter(|line| line.page == label.page)
        .filter_map(|line| {
            let headers = column_headers(&line.lower);
            if headers.len() != 1 {
                return None;
            }
            let slot = columns.iter().position(|column| *column == headers[0])?;
            let position = bounds(line, axis);
            (position[3] < label_bounds[2] && position[0] > label_bounds[1])
                .then_some((slot, line, position))
        })
        .collect::<Vec<_>>();
    // A missing or ambiguous header must never silently shift a value's column.
    if headers.len() != columns.len() || columns.is_empty() {
        return;
    }
    for slot in 0..columns.len() {
        if headers.iter().filter(|header| header.0 == slot).count() != 1 {
            return;
        }
    }
    for (slot, header, position) in &headers {
        let candidates = lines
            .iter()
            .filter(|line| {
                if line.page != label.page
                    || !line
                        .lower
                        .trim_start()
                        .starts_with(|c: char| c.is_ascii_digit() || c == '<')
                {
                    return false;
                }
                let cell = bounds(line, axis);
                if cell[0] <= label_bounds[1] || !aligned_row(label_bounds, cell) {
                    return false;
                }
                let center = (cell[0] + cell[1]) / 2.0;
                let distance = (center - (position[0] + position[1]) / 2.0).abs();
                if distance > (position[1] - position[0]).max(cell[1] - cell[0]) {
                    return false;
                }
                if headers.iter().any(|other| {
                    other.0 != *slot && (center - (other.2[0] + other.2[1]) / 2.0).abs() <= distance
                }) {
                    return false;
                }
                // Overlapping nutrient labels make attribution ambiguous.
                !lines.iter().any(|other| {
                    other.page == label.page
                        && other.observation.observation_id != label.observation.observation_id
                        && classify(&other.lower).is_some()
                        && aligned_row(bounds(other, axis), cell)
                })
            })
            .collect::<Vec<_>>();
        if let [cell] = candidates.as_slice() {
            let start = readings.len();
            read_row(row, label_unit, cell, readings);
            for reading in &mut readings[start..] {
                reading.slot = *slot;
                reading.lines = vec![label, cell, header];
            }
        }
    }
}

fn quantities(text: &str) -> Vec<Quantity> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut found = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() || (index > 0 && chars[index - 1].is_alphabetic()) {
            index += 1;
            continue;
        }
        let start = index;
        let mut number = String::new();
        while index < chars.len() {
            let c = chars[index];
            let next_is_digit = chars.get(index + 1).is_some_and(char::is_ascii_digit);
            if c.is_ascii_digit() {
                number.push(c);
            } else if c == '.' && next_is_digit {
                number.push('.');
            } else if c == ',' && next_is_digit {
                let digits = chars[index + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                if digits != 3 {
                    number.push('.');
                }
            } else {
                break;
            }
            index += 1;
        }
        let mut cursor = index;
        while chars.get(cursor) == Some(&' ') {
            cursor += 1;
        }
        let unit_start = cursor;
        while chars
            .get(cursor)
            .is_some_and(|c| c.is_alphabetic() || *c == '%')
        {
            cursor += 1;
        }
        let word = chars[unit_start..cursor].iter().collect::<String>();
        let unit = match word.as_str() {
            "" => None,
            "kj" => Some(Unit::Kj),
            "kcal" | "cal" | "calories" => Some(Unit::Kcal),
            "g" | "gm" | "grams" => Some(Unit::G),
            "mg" => Some(Unit::Mg),
            "mcg" | "µg" | "ug" => Some(Unit::Mcg),
            "ml" => Some(Unit::Ml),
            "%" => Some(Unit::Percent),
            _ => Some(Unit::Other),
        };
        if unit.is_some() {
            index = cursor;
        }
        let mut before = start;
        while before > 0 && chars[before - 1] == ' ' {
            before -= 1;
        }
        let upper_bound = chars[..before].ends_with(&['<'])
            || chars[before.saturating_sub(9)..before]
                .iter()
                .eq("less than".chars().collect::<Vec<_>>().iter());
        if let Ok(amount) = number.parse::<f64>() {
            found.push(Quantity {
                amount,
                unit,
                upper_bound,
            });
        }
    }
    found
}

// "Per serve (30g)" names the serving size inside the column header.
fn header_serving(text: &str) -> Option<Quantity> {
    let after = &text[text.find("per serv")? + "per serv".len()..];
    metric_quantity(&after[..after.find("per ").unwrap_or(after.len())])
}

fn metric_quantity(text: &str) -> Option<Quantity> {
    quantities(text)
        .into_iter()
        .rev()
        .find(|q| matches!(q.unit, Some(Unit::G | Unit::Ml)))
}

fn barcode(text: &str) -> Option<String> {
    let digits = text.split_whitespace().collect::<String>();
    if ![8, 12, 13].contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let values = digits
        .bytes()
        .map(|b| u32::from(b - b'0'))
        .collect::<Vec<_>>();
    let (body, check) = values.split_at(values.len() - 1);
    let sum = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, digit)| if i % 2 == 0 { digit * 3 } else { *digit })
        .sum::<u32>();
    ((10 - sum % 10) % 10 == check[0]).then_some(digits)
}

fn locale<'a>(lines: &'a [Line<'a>]) -> Option<(&'static str, &'a Line<'a>)> {
    [
        ("fssai", "en-IN"),
        ("nutrition facts", "en-US"),
        ("nutrition information", "en-AU"),
    ]
    .into_iter()
    .find_map(|(marker, locale)| {
        lines
            .iter()
            .find(|line| line.lower.contains(marker))
            .map(|line| (locale, line))
    })
}

// Reports what a reviewer should look at; the extracted values are never adjusted.
fn validate(
    output: &mut Output,
    columns: &[Column],
    values: &BTreeMap<(Column, Nutrient), f64>,
    serving: Option<(f64, Unit)>,
) {
    for column in columns {
        let get = |nutrient| values.get(&(*column, nutrient)).copied();
        if let (Some(kj), Some(kcal)) = (get(Nutrient::EnergyKj), get(Nutrient::EnergyKcal)) {
            if kcal > 0.0 && !(3.9..=4.5).contains(&(kj / kcal)) {
                output.fail("energy_unit_mismatch", ValidationSeverity::Warning);
            }
        }
        let energy =
            get(Nutrient::EnergyKcal).or(get(Nutrient::EnergyKj).map(|kj| kj / KJ_PER_KCAL));
        if let (Some(energy), Some(protein), Some(fat), Some(carbohydrate)) = (
            energy,
            get(Nutrient::Protein),
            get(Nutrient::Fat),
            get(Nutrient::Carbohydrate),
        ) {
            let atwater = 4.0 * protein + 4.0 * carbohydrate + 9.0 * fat;
            if (atwater - energy).abs() > (0.2 * energy).max(15.0) {
                output.fail("energy_atwater_mismatch", ValidationSeverity::Warning);
            }
        }
        if exceeds(get(Nutrient::SaturatedFat), get(Nutrient::Fat)) {
            output.fail("saturated_fat_exceeds_fat", ValidationSeverity::Warning);
        }
        if exceeds(get(Nutrient::Sugars), get(Nutrient::Carbohydrate)) {
            output.fail("sugars_exceed_carbohydrate", ValidationSeverity::Warning);
        }
        let solids = [
            Nutrient::Protein,
            Nutrient::Fat,
            Nutrient::Carbohydrate,
            Nutrient::Fibre,
        ]
        .into_iter()
        .filter_map(get)
        .sum::<f64>();
        if *column == Column::Per100g && solids > 101.0 {
            output.fail("per_100g_exceeds_100g", ValidationSeverity::Warning);
        }
    }

    let Some((size, unit)) = serving else { return };
    let reference = if unit == Unit::Ml {
        Column::Per100ml
    } else {
        Column::Per100g
    };
    if !columns.contains(&Column::PerServing) || !columns.contains(&reference) {
        return;
    }
    let mismatched = values.iter().any(|((column, nutrient), per_serving)| {
        *column == Column::PerServing
            && values.get(&(reference, *nutrient)).is_some_and(|per_100| {
                let expected = per_100 * size / 100.0;
                (per_serving - expected).abs() > (0.1 * expected).max(0.5)
            })
    });
    if mismatched {
        output.fail("serving_column_mismatch", ValidationSeverity::Warning);
    }
}

fn exceeds(part: Option<f64>, whole: Option<f64>) -> bool {
    matches!((part, whole), (Some(part), Some(whole)) if part > whole + 0.05)
}

fn round(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_thousands_separators_and_decimal_commas() {
        let parsed = quantities("energy 1,500 kj 0,5 g <1g");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].amount, 1500.0);
        assert_eq!(parsed[0].unit, Some(Unit::Kj));
        assert_eq!(parsed[1].amount, 0.5);
        assert!(parsed[2].upper_bound);
    }

    #[test]
    fn rejects_barcodes_with_a_wrong_check_digit() {
        assert_eq!(barcode("9300605000117").as_deref(), Some("9300605000117"));
        assert_eq!(barcode("9300605000118"), None);
        assert_eq!(barcode("10012345678901"), None);
    }
}
