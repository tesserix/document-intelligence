use ocr_domain::{
    Confidence, DocumentPage, NormalizedPoint, ObservationId, ObservationLevel, PageNumber,
    Polygon, TextObservation,
};

use crate::{
    assemble_recognitions, DetectionCandidate, Error, RecognizedRegion, Result, TransformChain,
};

pub fn build_page_observations(
    page: PageNumber,
    width: u32,
    height: u32,
    regions: &[DetectionCandidate],
    recognitions: Vec<RecognizedRegion>,
    transforms: &TransformChain,
    level: ObservationLevel,
) -> Result<DocumentPage> {
    let recognitions = assemble_recognitions(recognitions, regions.len())?;
    let mut pairs = regions
        .iter()
        .copied()
        .zip(recognitions)
        .collect::<Vec<_>>();
    pairs.sort_by(|(left, _), (right, _)| {
        let (left_x, left_y, _, _) = left.bounds();
        let (right_x, right_y, _, _) = right.bounds();
        left_y
            .total_cmp(&right_y)
            .then_with(|| left_x.total_cmp(&right_x))
    });
    let page_number = u32::from(page);
    let observations = pairs
        .into_iter()
        .enumerate()
        .map(|(reading_order, (region, recognition))| {
            let reading_order =
                u32::try_from(reading_order).map_err(|_| Error::InvalidPageObservation)?;
            let polygon = original_polygon(region, transforms)?;
            let observation_id = ObservationId::try_from(format!(
                "obs_p{page_number}_r{}",
                recognition.region_index()
            ))
            .map_err(|_| Error::InvalidPageObservation)?;
            let confidence = Confidence::new(recognition.confidence())
                .map_err(|_| Error::InvalidPageObservation)?;
            TextObservation::new(
                observation_id,
                level,
                recognition.text(),
                confidence,
                polygon,
                reading_order,
                None,
            )
            .map_err(|_| Error::InvalidPageObservation)
        })
        .collect::<Result<Vec<_>>>()?;
    DocumentPage::new(page, width, height, observations).map_err(|_| Error::InvalidPageObservation)
}

fn original_polygon(region: DetectionCandidate, transforms: &TransformChain) -> Result<Polygon> {
    let (left, top, right, bottom) = region.bounds();
    [(left, top), (right, top), (right, bottom), (left, bottom)]
        .into_iter()
        .map(|(x, y)| {
            let (x, y) = transforms.map_to_original(x, y)?;
            NormalizedPoint::new(x, y).map_err(|_| Error::InvalidPageObservation)
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|points| Polygon::new(points).map_err(|_| Error::InvalidPageObservation))
}
