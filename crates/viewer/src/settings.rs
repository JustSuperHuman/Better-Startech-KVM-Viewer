//! Local viewer preferences: picture positions per video mode only. No input,
//! clipboard, or target content is ever stored.
use crate::transport::SavedPosition;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 65_536;
const MAX_POSITIONS: usize = 64;

pub type Mode = (u16, u16, u8);

#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub positions: Vec<SavedPosition>,
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")?;
        Some(
            PathBuf::from(base)
                .join("USB Crash Cart")
                .join("settings.json"),
        )
    }

    /// A missing, oversized, or invalid file starts from defaults.
    pub fn load(path: &Path) -> Self {
        let read = || -> Result<Self> {
            let file = std::fs::File::open(path)?;
            ensure!(
                file.metadata()?.len() <= MAX_BYTES,
                "Settings file too large"
            );
            let settings: Self = serde_json::from_reader(std::io::BufReader::new(file))?;
            ensure!(
                settings.positions.len() <= MAX_POSITIONS,
                "Too many positions"
            );
            ensure!(
                settings.positions.iter().all(|position| {
                    (1..=1920).contains(&position.width)
                        && (1..=1600).contains(&position.height)
                        && position.hz > 0
                }),
                "Invalid saved position"
            );
            Ok(settings)
        };
        read().unwrap_or_default()
    }

    /// Write a sibling temporary file, then replace the original.
    pub fn save(&self, path: &Path) -> Result<()> {
        let directory = path.parent().context("Settings path has no folder")?;
        std::fs::create_dir_all(directory)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temporary, path).context("Replace viewer settings")
    }

    pub fn position(&self, mode: Mode) -> Option<(i8, i8)> {
        self.positions
            .iter()
            .find(|position| (position.width, position.height, position.hz) == mode)
            .map(|position| (position.x, position.y))
    }

    pub fn set_position(&mut self, mode: Mode, x: i8, y: i8) {
        self.clear_position(mode);
        if self.positions.len() >= MAX_POSITIONS {
            self.positions.remove(0);
        }
        self.positions.push(SavedPosition {
            width: mode.0,
            height: mode.1,
            hz: mode.2,
            x,
            y,
        });
    }

    pub fn clear_position(&mut self, mode: Mode) {
        self.positions
            .retain(|position| (position.width, position.height, position.hz) != mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_replace_and_reject_invalid_files() {
        let directory = std::env::temp_dir().join(format!("cart-settings-{}", std::process::id()));
        let path = directory.join("settings.json");
        let mut settings = Settings::default();
        settings.set_position((1920, 1080, 60), -45, 0);
        settings.set_position((1920, 1080, 60), -44, 1);
        settings.set_position((1280, 1024, 75), 3, -2);
        assert_eq!(settings.position((1920, 1080, 60)), Some((-44, 1)));
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        settings.clear_position((1280, 1024, 75));
        assert_eq!(settings.position((1280, 1024, 75)), None);
        std::fs::write(
            &path,
            r#"{"positions":[{"width":4000,"height":1080,"hz":60,"x":0,"y":0}]}"#,
        )
        .unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert_eq!(
            Settings::load(&directory.join("missing.json")),
            Settings::default()
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
