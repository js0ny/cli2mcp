use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs,
    path::Path,
};

use serde::Deserialize;

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) tools: Vec<ResolvedToolConfig>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    default: DefaultConfigPart,
    #[serde(default)]
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
pub(crate) enum ToolEffect {
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
pub(crate) struct ResolvedToolConfig {
    pub(crate) expose_name: String,
    pub(crate) description: String,
    pub(crate) exec: String,
    pub(crate) args: Vec<String>,
    pub(crate) timeout_ms: u64,
    pub(crate) effect: ToolEffect,
    pub(crate) cwd: Option<String>,
    pub(crate) cwd_fallback: Option<String>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) env_inherit: Vec<String>,
}

pub(crate) fn load(path: &Path) -> Result<Config, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let config: FileConfig = toml::from_str(&contents)?;

    if config.default.timeout_ms == 0 {
        return Err("default.timeout_ms must be greater than 0".into());
    }
    for tool in &config.tools {
        if let Some(0) = tool.timeout_ms {
            return Err(error_for(&tool.expose_name, "timeout_ms must be greater than 0").into());
        }
    }

    let tools: Vec<ResolvedToolConfig> = config
        .tools
        .into_iter()
        .map(|tool| merge_tool_config(tool, &config.default))
        .collect();

    let mut seen = HashSet::with_capacity(tools.len());
    for tool in &tools {
        if !is_valid_tool_name(&tool.expose_name) {
            return Err(error_for(
                &tool.expose_name,
                "expose_name must match [a-z0-9_.-]{1,128}",
            )
            .into());
        }
        if tool.exec.is_empty() {
            return Err(error_for(&tool.expose_name, "exec must not be empty").into());
        }
        if !seen.insert(tool.expose_name.as_str()) {
            return Err(format!("duplicate tool name '{}'", tool.expose_name).into());
        }
    }

    Ok(Config { tools })
}

fn error_for(name: &str, message: &str) -> String {
    format!("tool '{name}': {message}")
}

fn is_valid_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-.".contains(&byte)
        })
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
    use std::io::Write;

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
            timeout_ms = 700
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

        assert_eq!(overridden.timeout_ms, 700);
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

    fn load_str(contents: &str) -> Result<Config, String> {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        load(file.path()).map_err(|error| error.to_string())
    }

    #[test]
    fn zero_timeout_is_rejected_in_defaults_and_tools_even_when_overridden() {
        for contents in [
            "[default]\ntimeout_ms = 0\n".to_owned(),
            format!("[default]\ntimeout_ms = 0\n{TOOL}"),
            format!("{TOOL}\ntimeout_ms = 0\n"),
        ] {
            let error = load_str(&contents).unwrap_err();
            assert!(
                error.contains("timeout_ms must be greater than 0"),
                "{error}"
            );
        }

        let contents = format!("[default]\ntimeout_ms = 900\n{TOOL}\ntimeout_ms = 0\n");
        assert!(load_str(&contents).is_err());
    }

    #[test]
    fn invalid_names_execs_and_duplicates_are_rejected() {
        for (contents, expected) in [
            (
                TOOL.replace("expose_name = \"echo\"", "expose_name = \"Echo\""),
                "tool 'Echo': expose_name must match [a-z0-9_.-]{1,128}",
            ),
            (
                TOOL.replace("expose_name = \"echo\"", "expose_name = \"\""),
                "tool '': expose_name must match [a-z0-9_.-]{1,128}",
            ),
            (
                TOOL.replace("expose_name = \"echo\"", "expose_name = \"bad name\""),
                "tool 'bad name': expose_name must match [a-z0-9_.-]{1,128}",
            ),
            (
                TOOL.replace("exec = \"printf\"", "exec = \"\""),
                "tool 'echo': exec must not be empty",
            ),
            (format!("{TOOL}{TOOL}"), "duplicate tool name 'echo'"),
        ] {
            assert_eq!(load_str(&contents).unwrap_err(), expected);
        }
    }

    #[test]
    fn empty_tool_list_and_valid_names_load() {
        assert!(load_str("").unwrap().tools.is_empty());
        assert!(load_str("tools = []").is_ok());
        assert!(
            load_str(
                r#"
                [[tools]]
                expose_name = "ns.tool_2-name"
                description = "Valid"
                exec = "printf"
                "#
            )
            .is_ok()
        );
    }
}
