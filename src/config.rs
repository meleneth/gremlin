use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GremlinConfig {
    pub default_db: Option<PathBuf>,
    pub machine_label: Option<String>,
    pub jobs_limit: Option<i64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConfigFormat {
    Json,
}

#[derive(Debug, Clone)]
pub struct ConfigContext {
    pub config: GremlinConfig,
    pub path: Option<PathBuf>,
}

impl ConfigContext {
    pub fn resolve_db(&self, cli_db: Option<PathBuf>) -> anyhow::Result<PathBuf> {
        if let Some(path) = cli_db {
            return Ok(path);
        }
        if let Ok(path) = std::env::var("GREMLIN_DB") {
            return Ok(PathBuf::from(path));
        }
        if let Some(path) = self.config.default_db.clone() {
            return Ok(path);
        }
        anyhow::bail!(
            "no database path configured; pass --db, set GREMLIN_DB, or set default_db in {}",
            default_config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "a config file".to_string())
        );
    }

    pub fn jobs_limit(&self) -> i64 {
        self.config.jobs_limit.unwrap_or(200)
    }

    pub fn machine_label(&self, cli_label: Option<String>) -> Option<String> {
        cli_label
            .or_else(|| std::env::var("GREMLIN_MACHINE_LABEL").ok())
            .or_else(|| self.config.machine_label.clone())
    }
}

pub fn load(config_path: Option<PathBuf>, no_config: bool) -> anyhow::Result<ConfigContext> {
    if no_config {
        return Ok(ConfigContext {
            config: GremlinConfig::default(),
            path: None,
        });
    }

    let path = match config_path {
        Some(path) => path,
        None => match std::env::var("GREMLIN_CONFIG") {
            Ok(path) => PathBuf::from(path),
            Err(_) => match default_config_path() {
                Some(path) => path,
                None => {
                    return Ok(ConfigContext {
                        config: GremlinConfig::default(),
                        path: None,
                    })
                }
            },
        },
    };

    if !path.exists() {
        return Ok(ConfigContext {
            config: GremlinConfig::default(),
            path: Some(path),
        });
    }

    let text = std::fs::read_to_string(&path)?;
    let config = serde_json::from_str(&text)?;
    Ok(ConfigContext {
        config,
        path: Some(path),
    })
}

pub fn write_default(path: Option<PathBuf>, config: &GremlinConfig) -> anyhow::Result<PathBuf> {
    let path = match path {
        Some(path) => path,
        None => default_config_path()
            .ok_or_else(|| anyhow::anyhow!("could not determine default config path"))?,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, format!("{text}\n"))?;
    Ok(path)
}

pub fn default_config_path() -> Option<PathBuf> {
    if let Ok(base) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(base).join("gremlin").join("config.json"));
    }
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("gremlin")
            .join("config.json")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_cli_db_before_config_db() {
        let ctx = ConfigContext {
            config: GremlinConfig {
                default_db: Some(PathBuf::from("config.db")),
                ..GremlinConfig::default()
            },
            path: None,
        };
        assert_eq!(
            ctx.resolve_db(Some(PathBuf::from("cli.db"))).unwrap(),
            PathBuf::from("cli.db")
        );
    }
}
