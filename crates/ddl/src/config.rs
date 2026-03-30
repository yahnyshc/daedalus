use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

use crate::error::{DdlError, Result};

pub const CONFIG_FILE_NAME: &str = "config.json";
pub const DEFAULT_CONFIG_JSON: &str = r#"{
  "checkpointing": {
    "before": [
      "Edit(*)",
      "Write(*)",
      "Bash(npm install:*)",
      "Bash(git rebase:*)",
      "Bash(rm:*)",
      "Bash(mv:*)"
    ]
  }
}
"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaedalusConfig {
    pub checkpointing: CheckpointingConfig,
}

impl DaedalusConfig {
    pub fn parse(raw: &str) -> Result<Self> {
        let value = JsonParser::new(raw).parse()?;
        Self::from_json(value)
    }

    fn from_json(value: JsonValue) -> Result<Self> {
        let object = expect_object(value, "config root")?;
        let checkpointing =
            expect_object(take_required(&object, "checkpointing")?, "`checkpointing`")?;
        let before = expect_array(take_required(&checkpointing, "before")?, "`before`")?;

        let mut parsed_rules = Vec::with_capacity(before.len());
        for value in before {
            let raw = expect_string(value, "checkpoint rule")?;
            parsed_rules.push(CheckpointRule::parse(&raw)?);
        }

        Ok(Self {
            checkpointing: CheckpointingConfig {
                before: parsed_rules,
            },
        })
    }

    pub fn matching_bash_rule(&self, command: &[String]) -> Option<&CheckpointRule> {
        self.checkpointing
            .before
            .iter()
            .find(|rule| rule.matches_bash(command))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointingConfig {
    pub before: Vec<CheckpointRule>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointRule {
    pub raw: String,
    pub tool: ToolKind,
    pub matcher: RuleMatcher,
}

impl CheckpointRule {
    pub fn parse(raw: &str) -> Result<Self> {
        let (tool_name, pattern) = raw.split_once('(').ok_or_else(|| {
            DdlError::InvalidConfig(format!(
                "invalid checkpoint rule `{raw}`: expected Tool(pattern)"
            ))
        })?;
        if !pattern.ends_with(')') {
            return Err(DdlError::InvalidConfig(format!(
                "invalid checkpoint rule `{raw}`: missing closing `)`"
            )));
        }
        let pattern = &pattern[..pattern.len() - 1];
        let tool = ToolKind::parse(tool_name)?;
        let matcher = RuleMatcher::parse(&tool, pattern, raw)?;

        Ok(Self {
            raw: raw.to_string(),
            tool,
            matcher,
        })
    }

    pub fn matches_bash(&self, command: &[String]) -> bool {
        matches!(self.tool, ToolKind::Bash) && self.matcher.matches(command)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolKind {
    Bash,
    Edit,
    Write,
}

impl ToolKind {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "Bash" => Ok(Self::Bash),
            "Edit" => Ok(Self::Edit),
            "Write" => Ok(Self::Write),
            _ => Err(DdlError::InvalidConfig(format!(
                "invalid checkpoint rule tool `{value}`; expected Bash, Edit, or Write"
            ))),
        }
    }
}

impl Display for ToolKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bash => write!(f, "bash"),
            Self::Edit => write!(f, "edit"),
            Self::Write => write!(f, "write"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleMatcher {
    pub argv_prefix: Vec<String>,
    pub allow_trailing: bool,
}

impl RuleMatcher {
    fn parse(tool: &ToolKind, pattern: &str, raw_rule: &str) -> Result<Self> {
        match tool {
            ToolKind::Bash => {
                if pattern == "*" {
                    return Ok(Self {
                        argv_prefix: Vec::new(),
                        allow_trailing: true,
                    });
                }

                let (raw_prefix, allow_trailing) = match pattern.strip_suffix(":*") {
                    Some(prefix) => (prefix, true),
                    None => (pattern, false),
                };
                let argv_prefix = split_command_words(raw_prefix).map_err(|message| {
                    DdlError::InvalidConfig(format!(
                        "invalid checkpoint rule `{raw_rule}`: {message}"
                    ))
                })?;
                if argv_prefix.is_empty() {
                    return Err(DdlError::InvalidConfig(format!(
                        "invalid checkpoint rule `{raw_rule}`: command pattern must not be empty"
                    )));
                }

                Ok(Self {
                    argv_prefix,
                    allow_trailing,
                })
            }
            ToolKind::Edit | ToolKind::Write => {
                if pattern != "*" {
                    return Err(DdlError::InvalidConfig(format!(
                        "invalid checkpoint rule `{raw_rule}`: only `*` is accepted for {tool:?} in v1"
                    )));
                }
                Ok(Self {
                    argv_prefix: Vec::new(),
                    allow_trailing: true,
                })
            }
        }
    }

    fn matches(&self, command: &[String]) -> bool {
        if self.argv_prefix.is_empty() {
            return self.allow_trailing;
        }
        if command.len() < self.argv_prefix.len() {
            return false;
        }
        if !command.starts_with(&self.argv_prefix) {
            return false;
        }
        self.allow_trailing || command.len() == self.argv_prefix.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedCommand {
    pub argv: Vec<String>,
    pub command_string: String,
}

impl NormalizedCommand {
    pub fn from_argv(argv: Vec<String>) -> Self {
        let command_string = argv.join(" ");
        Self {
            argv,
            command_string,
        }
    }

    pub fn from_shell_args(shell_argv: &[String]) -> Self {
        if let Some(script) = extract_shell_script(shell_argv) {
            match split_command_words(&script) {
                Ok(argv) if !argv.is_empty() => Self {
                    command_string: argv.join(" "),
                    argv,
                },
                _ => Self {
                    argv: vec![script.clone()],
                    command_string: script,
                },
            }
        } else {
            Self::from_argv(shell_argv.to_vec())
        }
    }
}

fn extract_shell_script(argv: &[String]) -> Option<String> {
    let mut index = 0usize;
    while index < argv.len() {
        let item = &argv[index];
        if item == "-c" || item == "--command" {
            return argv.get(index + 1).cloned();
        }

        if item.starts_with('-') && item.len() > 2 && item[1..].contains('c') {
            return argv.get(index + 1).cloned();
        }

        if item.starts_with('-') {
            index += 1;
            continue;
        }

        break;
    }
    None
}

pub fn split_command_words(input: &str) -> std::result::Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            match ch {
                '\'' if active == '\'' => quote = None,
                '"' if active == '"' => quote = None,
                '\\' if active == '"' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        return Err("unterminated escape sequence".to_string());
                    }
                }
                _ => current.push(ch),
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    return Err("unterminated escape sequence".to_string());
                }
            }
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if quote.is_some() {
        return Err("unterminated quoted string".to_string());
    }

    if !current.is_empty() {
        words.push(current);
    }

    Ok(words)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum JsonValue {
    Object(BTreeMap<String, JsonValue>),
    Array(Vec<JsonValue>),
    String(String),
    Bool(bool),
    Null,
}

struct JsonParser<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            offset: 0,
        }
    }

    fn parse(mut self) -> Result<JsonValue> {
        self.skip_whitespace();
        let value = self.parse_value()?;
        self.skip_whitespace();
        if self.offset != self.input.len() {
            return Err(DdlError::InvalidConfig(format!(
                "unexpected trailing JSON content at byte {}",
                self.offset + 1
            )));
        }
        Ok(value)
    }

    fn parse_value(&mut self) -> Result<JsonValue> {
        self.skip_whitespace();
        let Some(byte) = self.peek() else {
            return Err(DdlError::InvalidConfig(
                "unexpected end of JSON input".to_string(),
            ));
        };

        match byte {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(JsonValue::String),
            b't' => self.parse_literal(b"true", JsonValue::Bool(true)),
            b'f' => self.parse_literal(b"false", JsonValue::Bool(false)),
            b'n' => self.parse_literal(b"null", JsonValue::Null),
            _ => Err(DdlError::InvalidConfig(format!(
                "unexpected JSON token at byte {}",
                self.offset + 1
            ))),
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue> {
        self.expect(b'{')?;
        let mut values = BTreeMap::new();
        self.skip_whitespace();
        if self.consume_if(b'}') {
            return Ok(JsonValue::Object(values));
        }

        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.parse_value()?;
            values.insert(key, value);
            self.skip_whitespace();
            if self.consume_if(b'}') {
                break;
            }
            self.expect(b',')?;
        }

        Ok(JsonValue::Object(values))
    }

    fn parse_array(&mut self) -> Result<JsonValue> {
        self.expect(b'[')?;
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.consume_if(b']') {
            return Ok(JsonValue::Array(values));
        }

        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            if self.consume_if(b']') {
                break;
            }
            self.expect(b',')?;
        }

        Ok(JsonValue::Array(values))
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut output = String::new();

        while let Some(byte) = self.next() {
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let escaped = self.next().ok_or_else(|| {
                        DdlError::InvalidConfig("unterminated JSON escape sequence".to_string())
                    })?;
                    match escaped {
                        b'"' => output.push('"'),
                        b'\\' => output.push('\\'),
                        b'/' => output.push('/'),
                        b'b' => output.push('\u{0008}'),
                        b'f' => output.push('\u{000c}'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => {
                            let codepoint = self.parse_unicode_escape()?;
                            let ch = char::from_u32(codepoint).ok_or_else(|| {
                                DdlError::InvalidConfig(format!(
                                    "invalid JSON unicode escape \\u{codepoint:04x}"
                                ))
                            })?;
                            output.push(ch);
                        }
                        other => {
                            return Err(DdlError::InvalidConfig(format!(
                                "invalid JSON escape `\\{}`",
                                other as char
                            )));
                        }
                    }
                }
                other => output.push(other as char),
            }
        }

        Err(DdlError::InvalidConfig(
            "unterminated JSON string".to_string(),
        ))
    }

    fn parse_unicode_escape(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.next().ok_or_else(|| {
                DdlError::InvalidConfig("unterminated JSON unicode escape".to_string())
            })?;
            value <<= 4;
            value |= match byte {
                b'0'..=b'9' => (byte - b'0') as u32,
                b'a'..=b'f' => (byte - b'a' + 10) as u32,
                b'A'..=b'F' => (byte - b'A' + 10) as u32,
                _ => {
                    return Err(DdlError::InvalidConfig(format!(
                        "invalid JSON unicode escape character `{}`",
                        byte as char
                    )));
                }
            };
        }
        Ok(value)
    }

    fn parse_literal(&mut self, literal: &[u8], value: JsonValue) -> Result<JsonValue> {
        for expected in literal {
            let actual = self.next().ok_or_else(|| {
                DdlError::InvalidConfig("unexpected end of JSON input".to_string())
            })?;
            if &actual != expected {
                return Err(DdlError::InvalidConfig(format!(
                    "unexpected JSON token at byte {}",
                    self.offset
                )));
            }
        }
        Ok(value)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.offset += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<()> {
        let actual = self
            .next()
            .ok_or_else(|| DdlError::InvalidConfig("unexpected end of JSON input".to_string()))?;
        if actual == byte {
            Ok(())
        } else {
            Err(DdlError::InvalidConfig(format!(
                "unexpected JSON token at byte {}",
                self.offset
            )))
        }
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.offset).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.offset += 1;
        Some(byte)
    }
}

fn take_required(map: &BTreeMap<String, JsonValue>, key: &str) -> Result<JsonValue> {
    map.get(key)
        .cloned()
        .ok_or_else(|| DdlError::InvalidConfig(format!("missing required config field `{key}`")))
}

fn expect_object(value: JsonValue, context: &str) -> Result<BTreeMap<String, JsonValue>> {
    match value {
        JsonValue::Object(map) => Ok(map),
        _ => Err(DdlError::InvalidConfig(format!(
            "{context} must be a JSON object"
        ))),
    }
}

fn expect_array(value: JsonValue, context: &str) -> Result<Vec<JsonValue>> {
    match value {
        JsonValue::Array(values) => Ok(values),
        _ => Err(DdlError::InvalidConfig(format!(
            "{context} must be a JSON array"
        ))),
    }
}

fn expect_string(value: JsonValue, context: &str) -> Result<String> {
    match value {
        JsonValue::String(value) => Ok(value),
        _ => Err(DdlError::InvalidConfig(format!(
            "{context} must be a JSON string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckpointRule, DEFAULT_CONFIG_JSON, DaedalusConfig, NormalizedCommand, ToolKind};

    #[test]
    fn parses_default_json_config() {
        let config = DaedalusConfig::parse(DEFAULT_CONFIG_JSON).expect("parse config");
        assert_eq!(config.checkpointing.before.len(), 6);
        assert_eq!(config.checkpointing.before[0].tool, ToolKind::Edit);
        assert_eq!(config.checkpointing.before[2].tool, ToolKind::Bash);
    }

    #[test]
    fn rejects_malformed_rule_string() {
        let error = CheckpointRule::parse("Bash(npm install:*").expect_err("rule should fail");
        assert!(error.to_string().contains("missing closing `)`"));
    }

    #[test]
    fn parses_prefix_and_exact_bash_rules() {
        let prefix = CheckpointRule::parse("Bash(npm install:*)").expect("parse prefix rule");
        assert_eq!(prefix.matcher.argv_prefix, vec!["npm", "install"]);
        assert!(prefix.matcher.allow_trailing);

        let exact = CheckpointRule::parse("Bash(git status)").expect("parse exact rule");
        assert_eq!(exact.matcher.argv_prefix, vec!["git", "status"]);
        assert!(!exact.matcher.allow_trailing);
    }

    #[test]
    fn matches_shell_command_prefixes() {
        let config = DaedalusConfig::parse(DEFAULT_CONFIG_JSON).expect("parse config");

        assert!(
            config
                .matching_bash_rule(&vec!["npm".into(), "install".into(), "foo".into()])
                .is_some()
        );
        assert!(
            config
                .matching_bash_rule(&vec!["git".into(), "rebase".into(), "main".into()])
                .is_some()
        );
        assert!(
            config
                .matching_bash_rule(&vec!["rm".into(), "-rf".into(), "tmp".into()])
                .is_some()
        );
        assert!(
            config
                .matching_bash_rule(&vec!["ls".into(), "-la".into()])
                .is_none()
        );
    }

    #[test]
    fn normalizes_shell_script_commands() {
        let normalized =
            NormalizedCommand::from_shell_args(&["-lc".to_string(), "npm install foo".to_string()]);
        assert_eq!(normalized.argv, vec!["npm", "install", "foo"]);
        assert_eq!(normalized.command_string, "npm install foo");
    }
}
