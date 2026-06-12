use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

const DEFAULT_BASE_URL: &str = "https://app.carrac.cc";

/// Which OHLCV backend the client talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Authenticated Carrac platform — full feature set. The default whenever
    /// an API key is configured.
    #[default]
    Carrac,
    /// Keyless public Binance backend — chart-only demo / bring-your-own-backend.
    Binance,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// API key for the Carrac backend. Empty string in keyless / Binance mode
    /// (kept as `String` rather than `Option` so `Client::new` is unchanged —
    /// the demo never sends authenticated requests anyway).
    pub api_key: String,
    pub base_url: String,
    /// Selected OHLCV backend. Defaults to Carrac when a key is present, else
    /// Binance (keyless demo).
    pub backend: Backend,
    pub agent: AgentConfig,
}

impl Config {
    /// True when no Carrac API key is configured — the client runs in the
    /// keyless Binance demo and Carrac-only surfaces are gated.
    pub fn is_demo(&self) -> bool {
        self.backend == Backend::Binance
    }
}

/// The one-liner every Carrac-only surface prints in the keyless demo. Single
/// source of truth; the TUI widgets re-export this as `GATED_DEMO_NOTICE`.
pub const DEMO_GATE_NOTICE: &str = "needs a Carrac API key — carrac.cc/reg-tui";

/// CLI guard: in the keyless demo print the gate notice for `command` and
/// return `true` (caller should return Ok early). Returns `false` on the full
/// Carrac backend so the command proceeds normally.
pub fn gate_cli_if_demo(cfg: &Config, command: &str) -> bool {
    if cfg.is_demo() {
        println!("carrac {command}: {DEMO_GATE_NOTICE}");
        true
    } else {
        false
    }
}

/// Configuration for the local AI agent launched via `A`.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    /// Command to run (e.g. "claude", "aider", "gemini"). Default: "claude".
    pub command: String,
    /// Extra arguments to pass after the command (default: empty).
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    api_key: Option<String>,
    base_url: Option<String>,
    /// "carrac" | "binance" — overrides the key-presence default.
    backend: Option<String>,
    agent: Option<FileAgentConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct FileAgentConfig {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let file = read_file_config().unwrap_or_default();
        Ok(Self::resolve(file, EnvSource::from_process()))
    }

    /// Pure resolution of file + env into a `Config`. Split out from `load()`
    /// so the precedence rules are unit-testable without touching the real
    /// environment or filesystem.
    fn resolve(file: FileConfig, env: EnvSource) -> Self {
        // Env overrides (CARRAC_*) take precedence over the config file.
        let env_key = env.key;
        let env_url = env.url;

        let api_key = env_key.or(file.api_key).unwrap_or_default();
        let base_url = env_url
            .or(file.base_url)
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        // Backend: explicit config wins; otherwise default to Carrac iff a key
        // is present, else the keyless Binance demo.
        let backend = match file.backend.as_deref().map(|s| s.trim().to_lowercase()) {
            Some(ref b) if b == "binance" => Backend::Binance,
            Some(ref b) if b == "carrac" => Backend::Carrac,
            _ => {
                if api_key.is_empty() {
                    Backend::Binance
                } else {
                    Backend::Carrac
                }
            }
        };

        let agent = {
            let cfg = file.agent.unwrap_or_default();
            AgentConfig {
                command: cfg.command.unwrap_or_else(|| "claude".to_string()),
                args: cfg.args,
            }
        };

        Self {
            api_key,
            base_url,
            backend,
            agent,
        }
    }
}

/// Resolved environment overrides (CARRAC_*).
struct EnvSource {
    key: Option<String>,
    url: Option<String>,
}

impl EnvSource {
    fn from_process() -> Self {
        let getenv = |name: &str| std::env::var(name).ok().filter(|s| !s.is_empty());
        Self {
            key: getenv("CARRAC_API_KEY"),
            url: getenv("CARRAC_BASE_URL"),
        }
    }
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("cc", "carrac", "carrac")?;
    Some(dirs.config_dir().join("config.toml"))
}

/// Return the config file path as a display string for user-facing messages.
pub fn config_path_display() -> String {
    config_path().map_or_else(
        || "~/.config/carrac/config.toml".to_string(),
        |p| p.display().to_string(),
    )
}

fn read_file_config() -> Option<FileConfig> {
    let path = config_path()?;
    let body = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env() -> EnvSource {
        EnvSource {
            key: None,
            url: None,
        }
    }

    #[test]
    fn parses_minimal_toml() {
        let c: FileConfig = toml::from_str(r#"api_key = "k""#).unwrap();
        assert_eq!(c.api_key.as_deref(), Some("k"));
        assert!(c.base_url.is_none());
        assert!(c.agent.is_none());
    }

    #[test]
    fn parses_agent_section_command_and_args() {
        let c: FileConfig = toml::from_str(
            r#"
api_key = "k"
[agent]
command = "aider"
args = ["--no-auto-commits"]
"#,
        )
        .unwrap();
        let ag = c.agent.unwrap();
        assert_eq!(ag.command.as_deref(), Some("aider"));
        assert_eq!(ag.args, vec!["--no-auto-commits"]);
    }

    #[test]
    fn agent_default_command_is_claude() {
        // When no [agent] section is present the default command must be "claude".
        let cfg = FileAgentConfig::default();
        let resolved = AgentConfig {
            command: cfg.command.unwrap_or_else(|| "claude".to_string()),
            args: cfg.args,
        };
        assert_eq!(resolved.command, "claude");
        assert!(resolved.args.is_empty());
    }

    #[test]
    fn agent_explicit_command_overrides_default() {
        let cfg = FileAgentConfig {
            command: Some("gemini".to_string()),
            args: vec![],
        };
        let resolved = AgentConfig {
            command: cfg.command.unwrap_or_else(|| "claude".to_string()),
            args: cfg.args,
        };
        assert_eq!(resolved.command, "gemini");
    }

    // ── keyless / backend resolution ────────────────────────────────────────

    #[test]
    fn no_key_anywhere_defaults_to_binance_demo() {
        let cfg = Config::resolve(FileConfig::default(), no_env());
        assert!(cfg.api_key.is_empty());
        assert_eq!(cfg.backend, Backend::Binance);
        assert!(cfg.is_demo());
        // Base URL still resolves to the default (harmless — demo never calls it).
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn key_present_defaults_to_carrac() {
        let file = FileConfig {
            api_key: Some("k".into()),
            ..Default::default()
        };
        let cfg = Config::resolve(file, no_env());
        assert_eq!(cfg.backend, Backend::Carrac);
        assert!(!cfg.is_demo());
    }

    #[test]
    fn carrac_env_key_wins_over_file() {
        let file = FileConfig {
            api_key: Some("file-key".into()),
            ..Default::default()
        };
        let env = EnvSource {
            key: Some("env-key".into()),
            url: None,
        };
        let cfg = Config::resolve(file, env);
        assert_eq!(cfg.api_key, "env-key");
        assert_eq!(cfg.backend, Backend::Carrac);
    }

    #[test]
    fn explicit_backend_binance_overrides_key_presence() {
        // A user with a key who explicitly wants the demo backend.
        let file = FileConfig {
            api_key: Some("k".into()),
            backend: Some("binance".into()),
            ..Default::default()
        };
        let cfg = Config::resolve(file, no_env());
        assert_eq!(cfg.backend, Backend::Binance);
    }

    #[test]
    fn explicit_backend_carrac_with_no_key() {
        // Pinning carrac without a key is allowed (requests will simply 401);
        // the user asked for it explicitly.
        let file = FileConfig {
            backend: Some("carrac".into()),
            ..Default::default()
        };
        let cfg = Config::resolve(file, no_env());
        assert_eq!(cfg.backend, Backend::Carrac);
    }

    #[test]
    fn carrac_base_url_env_overrides_file() {
        let file = FileConfig {
            api_key: Some("k".into()),
            base_url: Some("https://file.example".into()),
            ..Default::default()
        };
        let env = EnvSource {
            key: None,
            url: Some("https://env.example".into()),
        };
        let cfg = Config::resolve(file, env);
        assert_eq!(cfg.base_url, "https://env.example");
    }
}
