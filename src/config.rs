use std::{collections::HashMap, error::Error, fs};

use serde::Deserialize;

#[derive(Debug)]
pub struct Config {
    tools: Vec<ResolvedToolConfig>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    default: DefaultConfigPart,
    tools: Vec<ToolConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct DefaultConfigPart {
    timeout_ms: u64,
    effect: ToolEffect,
    cwd: Option<String>,
    cwd_fallback: Option<String>,
    env: HashMap<String, String>,
    env_inherit: Vec<String>,
}

impl Default for DefaultConfigPart {
    fn default() -> Self {
        Self {
            timeout_ms: 500,
            effect: ToolEffect::default(),
            cwd: None,
            cwd_fallback: None,
            env: HashMap::new(),
            env_inherit: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ToolEffect {
    Ro,
    Write,
    #[default]
    Destructive,
}

#[derive(Debug, Deserialize)]
struct ToolConfig {
    expose_name: String, // Lowercase only
    description: String,

    exec: String,
    args: Option<Vec<String>>,

    timeout_ms: Option<u64>,
    effect: Option<ToolEffect>,
    cwd: Option<String>,
    env: Option<HashMap<String, String>>,
    // An explicit empty list disables inheritance rather than using the defaults.
    env_inherit: Option<Vec<String>>,
}

#[derive(Debug)]
struct ResolvedToolConfig {
    expose_name: String,
    description: String,
    exec: String,
    args: Vec<String>,
    timeout_ms: u64,
    effect: ToolEffect,
    cwd: Option<String>,
    cwd_fallback: Option<String>,
    env: HashMap<String, String>,
    env_inherit: Vec<String>,
}

pub fn load(path: &str) -> Result<Config, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let config: FileConfig = toml::from_str(&contents)?;
    let tools = config
        .tools
        .into_iter()
        .map(|tool| merge_tool_config(tool, &config.default))
        .collect();

    Ok(Config { tools })
}

fn merge_tool_config(tool: ToolConfig, defaults: &DefaultConfigPart) -> ResolvedToolConfig {
    let mut env = defaults.env.clone();
    env.extend(tool.env.unwrap_or_default());

    ResolvedToolConfig {
        expose_name: tool.expose_name,
        description: tool.description,
        exec: tool.exec,
        args: tool.args.unwrap_or_default(),
        timeout_ms: tool.timeout_ms.unwrap_or(defaults.timeout_ms),
        effect: tool.effect.unwrap_or(defaults.effect),
        cwd: tool.cwd.or_else(|| defaults.cwd.clone()),
        cwd_fallback: defaults.cwd_fallback.clone(),
        env,
        env_inherit: tool
            .env_inherit
            .unwrap_or_else(|| defaults.env_inherit.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOL: &str = r#"
        [[tools]]
        expose_name = "echo"
        description = "Print a message"
        exec = "printf"
    "#;

    #[test]
    fn missing_default_table_or_fields_use_built_in_values() {
        for prefix in ["", "[default]\n"] {
            let config: FileConfig = toml::from_str(&format!("{prefix}{TOOL}")).unwrap();
            let tool = merge_tool_config(config.tools.into_iter().next().unwrap(), &config.default);

            assert_eq!(tool.timeout_ms, 500);
            assert_eq!(tool.effect, ToolEffect::Destructive);
        }
    }

    #[test]
    fn tool_overrides_do_not_change_defaults_for_other_tools() {
        let contents = r#"
            [default]
            timeout_ms = 900
            effect = "ro"
            cwd = "/default"
            cwd_fallback = "/fallback"
            env = { SHARED = "default", DEFAULT_ONLY = "kept" }

            [[tools]]
            expose_name = "override"
            description = "Override shared settings"
            exec = "printf"
            timeout_ms = 0
            effect = "write"
            cwd = "/tool"
            env = { SHARED = "tool", TOOL_ONLY = "added" }

            [[tools]]
            expose_name = "inherit"
            description = "Inherit shared settings"
            exec = "printf"
        "#;
        let config: FileConfig = toml::from_str(contents).unwrap();
        let mut tools = config.tools.into_iter();
        let overridden = merge_tool_config(tools.next().unwrap(), &config.default);
        let inherited = merge_tool_config(tools.next().unwrap(), &config.default);

        assert_eq!(overridden.timeout_ms, 0);
        assert_eq!(overridden.effect, ToolEffect::Write);
        assert_eq!(overridden.cwd.as_deref(), Some("/tool"));
        assert_eq!(overridden.cwd_fallback.as_deref(), Some("/fallback"));
        assert_eq!(
            overridden.env,
            HashMap::from([
                ("SHARED".into(), "tool".into()),
                ("DEFAULT_ONLY".into(), "kept".into()),
                ("TOOL_ONLY".into(), "added".into()),
            ])
        );
        assert_eq!(inherited.timeout_ms, 900);
        assert_eq!(inherited.effect, ToolEffect::Ro);
        assert_eq!(inherited.cwd.as_deref(), Some("/default"));
        assert_eq!(inherited.cwd_fallback.as_deref(), Some("/fallback"));
        assert_eq!(
            inherited.env,
            HashMap::from([
                ("SHARED".into(), "default".into()),
                ("DEFAULT_ONLY".into(), "kept".into()),
            ])
        );
    }

    #[test]
    fn env_inherit_distinguishes_omission_replacement_and_clearing() {
        for (setting, expected) in [
            ("", vec!["PATH", "HOME"]),
            ("env_inherit = [\"LANG\"]", vec!["LANG"]),
            ("env_inherit = []", vec![]),
        ] {
            let contents =
                format!("[default]\nenv_inherit = [\"PATH\", \"HOME\"]\n{TOOL}\n{setting}");
            let config: FileConfig = toml::from_str(&contents).unwrap();
            let tool = merge_tool_config(config.tools.into_iter().next().unwrap(), &config.default);

            assert_eq!(tool.env_inherit, expected);
        }
    }
}
