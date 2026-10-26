//! Bounded frame metadata checks only. No shared-memory layout, handles, or
//! operating-system IPC is defined here. Dimensions describe the active image;
//! allocation capacity must never be substituted for its width or height.

use std::fmt;

pub const MAX_FRAME_WIDTH: u32 = 1920;
pub const MAX_FRAME_HEIGHT: u32 = 1600;
pub const RGBA8_BYTES_PER_PIXEL: usize = 4;
pub const MAX_FRAME_CAPACITY: usize =
    MAX_FRAME_WIDTH as usize * MAX_FRAME_HEIGHT as usize * RGBA8_BYTES_PER_PIXEL;

/// RGBA8 rows. `stride_bytes` includes row padding; the buffer contract requires
/// a full `stride_bytes * height` allocation, including the last row's padding.
/// This is metadata, not a native-memory or wire ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrameMetadata {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    EmptyFrame,
    DimensionsExceedLimit { width: u32, height: u32 },
    StrideTooSmall { minimum: u64, received: u64 },
    ArithmeticOverflow,
    CapacityExceeded { required: usize, capacity: usize },
    NonIncreasingGeneration { previous: u64, received: u64 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => f.write_str("active frame dimensions must be nonzero"),
            Self::DimensionsExceedLimit { width, height } => write!(
                f,
                "active frame {width}x{height} exceeds {MAX_FRAME_WIDTH}x{MAX_FRAME_HEIGHT}"
            ),
            Self::StrideTooSmall { minimum, received } => {
                write!(f, "RGBA8 stride {received} is smaller than {minimum} bytes")
            }
            Self::ArithmeticOverflow => {
                f.write_str("frame byte layout overflows the addressable size")
            }
            Self::CapacityExceeded { required, capacity } => write!(
                f,
                "frame requires {required} bytes but capacity is {capacity}"
            ),
            Self::NonIncreasingGeneration { previous, received } => {
                write!(f, "frame generation {received} does not follow {previous}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// A validated layout. Private fields prevent constructing one without checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLayout {
    metadata: FrameMetadata,
    stride_bytes: usize,
    byte_len: usize,
}

impl FrameLayout {
    pub fn metadata(&self) -> FrameMetadata {
        self.metadata
    }

    pub fn stride_bytes(&self) -> usize {
        self.stride_bytes
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }
}

impl FrameMetadata {
    /// `capacity_bytes` is available pixel storage, excluding headers. Storage
    /// above the fixed maximum cannot relax frame bounds. `previous_generation`
    /// belongs to the same producer session; pass `None` for a new session.
    /// Generations must strictly increase and must not wrap within a session.
    pub fn validate(
        self,
        capacity_bytes: usize,
        previous_generation: Option<u64>,
    ) -> Result<FrameLayout, FrameError> {
        if self.width == 0 || self.height == 0 {
            return Err(FrameError::EmptyFrame);
        }
        if self.width > MAX_FRAME_WIDTH || self.height > MAX_FRAME_HEIGHT {
            return Err(FrameError::DimensionsExceedLimit {
                width: self.width,
                height: self.height,
            });
        }
        let minimum_stride = u64::from(self.width)
            .checked_mul(RGBA8_BYTES_PER_PIXEL as u64)
            .ok_or(FrameError::ArithmeticOverflow)?;
        if self.stride_bytes < minimum_stride {
            return Err(FrameError::StrideTooSmall {
                minimum: minimum_stride,
                received: self.stride_bytes,
            });
        }
        let byte_len = self
            .stride_bytes
            .checked_mul(u64::from(self.height))
            .ok_or(FrameError::ArithmeticOverflow)?;
        let byte_len = usize::try_from(byte_len).map_err(|_| FrameError::ArithmeticOverflow)?;
        let stride_bytes =
            usize::try_from(self.stride_bytes).map_err(|_| FrameError::ArithmeticOverflow)?;
        let capacity = capacity_bytes.min(MAX_FRAME_CAPACITY);
        if byte_len > capacity {
            return Err(FrameError::CapacityExceeded {
                required: byte_len,
                capacity,
            });
        }
        if let Some(previous) = previous_generation
            && self.generation <= previous
        {
            return Err(FrameError::NonIncreasingGeneration {
                previous,
                received: self.generation,
            });
        }
        Ok(FrameLayout {
            metadata: self,
            stride_bytes,
            byte_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hd() -> FrameMetadata {
        FrameMetadata {
            generation: 1,
            width: 1920,
            height: 1080,
            stride_bytes: 1920 * 4,
        }
    }

    #[test]
    fn capacity_is_not_active_size() {
        assert_eq!(MAX_FRAME_CAPACITY, 12_288_000);
        let layout = hd().validate(MAX_FRAME_CAPACITY, None).unwrap();
        assert_eq!(layout.metadata().width, 1920);
        assert_eq!(layout.metadata().height, 1080);
        assert_eq!(layout.byte_len(), 8_294_400);
        assert_eq!(layout.stride_bytes(), 7680);
        assert!(layout.byte_len() < MAX_FRAME_CAPACITY);
    }

    #[test]
    fn exact_maximum_and_single_pixel_frames_are_valid() {
        let maximum = FrameMetadata {
            height: MAX_FRAME_HEIGHT,
            ..hd()
        };
        assert_eq!(
            maximum
                .validate(MAX_FRAME_CAPACITY, None)
                .unwrap()
                .byte_len(),
            MAX_FRAME_CAPACITY
        );
        let single = FrameMetadata {
            generation: 0,
            width: 1,
            height: 1,
            stride_bytes: 4,
        };
        assert_eq!(single.validate(4, None).unwrap().byte_len(), 4);
    }

    #[test]
    fn padded_rows_are_valid_with_sufficient_storage() {
        let metadata = FrameMetadata {
            generation: 1,
            width: 3,
            height: 2,
            stride_bytes: 13,
        };
        let layout = metadata.validate(26, None).unwrap();
        assert_eq!(layout.stride_bytes(), 13);
        assert_eq!(layout.byte_len(), 26);
        assert_eq!(
            metadata.validate(25, None),
            Err(FrameError::CapacityExceeded {
                required: 26,
                capacity: 25
            })
        );
    }

    #[test]
    fn empty_and_oversized_active_dimensions_are_rejected() {
        for metadata in [
            FrameMetadata { width: 0, ..hd() },
            FrameMetadata { height: 0, ..hd() },
        ] {
            assert_eq!(
                metadata.validate(MAX_FRAME_CAPACITY, None),
                Err(FrameError::EmptyFrame)
            );
        }
        for metadata in [
            FrameMetadata {
                width: MAX_FRAME_WIDTH + 1,
                ..hd()
            },
            FrameMetadata {
                height: MAX_FRAME_HEIGHT + 1,
                ..hd()
            },
            FrameMetadata {
                width: u32::MAX,
                height: u32::MAX,
                ..hd()
            },
        ] {
            assert_eq!(
                metadata.validate(usize::MAX, None),
                Err(FrameError::DimensionsExceedLimit {
                    width: metadata.width,
                    height: metadata.height
                })
            );
        }
    }

    #[test]
    fn short_stride_and_short_storage_are_rejected() {
        let metadata = FrameMetadata {
            stride_bytes: 7679,
            ..hd()
        };
        assert_eq!(
            metadata.validate(MAX_FRAME_CAPACITY, None),
            Err(FrameError::StrideTooSmall {
                minimum: 7680,
                received: 7679
            })
        );
        assert_eq!(
            hd().validate(0, None),
            Err(FrameError::CapacityExceeded {
                required: 8_294_400,
                capacity: 0
            })
        );
        assert_eq!(
            hd().validate(8_294_399, None),
            Err(FrameError::CapacityExceeded {
                required: 8_294_400,
                capacity: 8_294_399
            })
        );
        assert_eq!(
            hd().validate(8_294_400, None).unwrap().byte_len(),
            8_294_400
        );
    }

    #[test]
    fn excess_storage_cannot_relax_the_maximum_frame_capacity() {
        let metadata = FrameMetadata {
            height: MAX_FRAME_HEIGHT,
            stride_bytes: 7681,
            ..hd()
        };
        assert_eq!(
            metadata.validate(usize::MAX, None),
            Err(FrameError::CapacityExceeded {
                required: 12_289_600,
                capacity: MAX_FRAME_CAPACITY
            })
        );
    }

    #[test]
    fn byte_count_overflow_is_rejected_before_access() {
        let metadata = FrameMetadata {
            height: 2,
            stride_bytes: u64::MAX,
            ..hd()
        };
        assert_eq!(
            metadata.validate(usize::MAX, None),
            Err(FrameError::ArithmeticOverflow)
        );
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn layouts_that_do_not_fit_a_32_bit_address_space_are_rejected() {
        let metadata = FrameMetadata {
            height: 1,
            stride_bytes: u64::from(u32::MAX) + 1,
            ..hd()
        };
        assert_eq!(
            metadata.validate(usize::MAX, None),
            Err(FrameError::ArithmeticOverflow)
        );
    }

    #[test]
    fn generations_must_increase_within_a_producer_session() {
        for received in [0, 5, 10] {
            let metadata = FrameMetadata {
                generation: received,
                ..hd()
            };
            assert_eq!(
                metadata.validate(MAX_FRAME_CAPACITY, Some(10)),
                Err(FrameError::NonIncreasingGeneration {
                    previous: 10,
                    received
                })
            );
        }
        assert!(
            FrameMetadata {
                generation: 11,
                ..hd()
            }
            .validate(MAX_FRAME_CAPACITY, Some(10))
            .is_ok()
        );
        assert!(
            FrameMetadata {
                generation: 0,
                ..hd()
            }
            .validate(MAX_FRAME_CAPACITY, None)
            .is_ok()
        );
    }

    #[test]
    fn generation_wrap_is_not_silently_accepted() {
        let metadata = FrameMetadata {
            generation: 0,
            ..hd()
        };
        assert_eq!(
            metadata.validate(MAX_FRAME_CAPACITY, Some(u64::MAX)),
            Err(FrameError::NonIncreasingGeneration {
                previous: u64::MAX,
                received: 0
            })
        );
    }
}
