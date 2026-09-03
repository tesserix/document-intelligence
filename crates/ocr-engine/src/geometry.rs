use crate::{Error, Result};
use serde::{Deserialize, Serialize};

const MAXIMUM_DESKEW_DEGREES: f64 = 12.0;

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawDeskewAngle")]
pub struct DeskewAngle {
    degrees: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeskewAngle {
    degrees: f64,
}

impl TryFrom<RawDeskewAngle> for DeskewAngle {
    type Error = Error;

    fn try_from(value: RawDeskewAngle) -> Result<Self> {
        Self::new(value.degrees)
    }
}

impl DeskewAngle {
    pub fn new(degrees: f64) -> Result<Self> {
        if degrees.is_finite()
            && (-MAXIMUM_DESKEW_DEGREES..=MAXIMUM_DESKEW_DEGREES).contains(&degrees)
        {
            Ok(Self { degrees })
        } else {
            Err(Error::InvalidDeskewAngle)
        }
    }

    pub fn degrees(self) -> f64 {
        self.degrees
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawCropRegion")]
pub struct CropRegion {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCropRegion {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

impl TryFrom<RawCropRegion> for CropRegion {
    type Error = Error;

    fn try_from(value: RawCropRegion) -> Result<Self> {
        Self::new(value.left, value.top, value.width, value.height)
    }
}

impl CropRegion {
    pub fn new(left: f64, top: f64, width: f64, height: f64) -> Result<Self> {
        let values = [left, top, width, height];
        if values.iter().any(|value| !value.is_finite())
            || left < 0.0
            || top < 0.0
            || width <= 0.0
            || height <= 0.0
            || left + width > 1.0
            || top + height > 1.0
        {
            return Err(Error::InvalidGeometry);
        }
        Ok(Self {
            left,
            top,
            width,
            height,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    #[serde(rename = "clockwise_90")]
    Clockwise90,
    #[serde(rename = "clockwise_180")]
    Clockwise180,
    #[serde(rename = "clockwise_270")]
    Clockwise270,
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransformStep {
    Crop { region: CropRegion },
    Deskew { angle: DeskewAngle },
    FlipHorizontal,
    FlipVertical,
    Rotation { rotation: Rotation },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformChain {
    steps: Vec<TransformStep>,
}

impl TransformChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_crop(&mut self, crop: CropRegion) {
        self.steps.push(TransformStep::Crop { region: crop });
    }

    pub fn push_deskew(&mut self, angle: DeskewAngle) {
        self.steps.push(TransformStep::Deskew { angle });
    }

    pub fn push_rotation(&mut self, rotation: Rotation) {
        self.steps.push(TransformStep::Rotation { rotation });
    }

    pub fn push_flip_horizontal(&mut self) {
        self.steps.push(TransformStep::FlipHorizontal);
    }

    pub fn push_flip_vertical(&mut self) {
        self.steps.push(TransformStep::FlipVertical);
    }

    pub fn steps(&self) -> &[TransformStep] {
        &self.steps
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn map_to_original(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        if !valid_coordinate(x) || !valid_coordinate(y) {
            return Err(Error::InvalidGeometry);
        }
        let (x, y) = self
            .steps
            .iter()
            .rev()
            .fold((x, y), |(x, y), transform| match transform {
                TransformStep::Crop { region } => (
                    region.left + x * region.width,
                    region.top + y * region.height,
                ),
                TransformStep::Deskew { angle } => {
                    let radians = angle.degrees().to_radians();
                    let translated_x = x - 0.5;
                    let translated_y = y - 0.5;
                    (
                        0.5 + radians.cos() * translated_x + radians.sin() * translated_y,
                        0.5 - radians.sin() * translated_x + radians.cos() * translated_y,
                    )
                }
                TransformStep::FlipHorizontal => (1.0 - x, y),
                TransformStep::FlipVertical => (x, 1.0 - y),
                TransformStep::Rotation {
                    rotation: Rotation::Clockwise90,
                } => (y, 1.0 - x),
                TransformStep::Rotation {
                    rotation: Rotation::Clockwise180,
                } => (1.0 - x, 1.0 - y),
                TransformStep::Rotation {
                    rotation: Rotation::Clockwise270,
                } => (1.0 - y, x),
            });
        if valid_coordinate(x) && valid_coordinate(y) {
            Ok((x, y))
        } else {
            Err(Error::InvalidGeometry)
        }
    }
}

fn valid_coordinate(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
