use std::io::ErrorKind;

use serde::{Deserialize, Serialize};

use crate::NDSConfig;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub name: [Option<String>; 3],
    pub icon: Option<String>,
}

impl Config {
    pub fn try_load(nds_config: &NDSConfig) -> std::io::Result<Self> {
        let mut path = nds_config.cargo_manifest_path.clone();
        path.pop();
        path.push("nds.toml");

        match std::fs::exists(&path) {
            Ok(true) => {}
            Ok(false) => return Ok(Self::default()),
            Err(e) => return Err(e),
        }

        let config = std::fs::read_to_string(&path)?;

        let config: Config = toml::from_str(&config)
            .map_err(|e| std::io::Error::new(ErrorKind::Other, e.message()))?;

        Ok(config)
    }
}
