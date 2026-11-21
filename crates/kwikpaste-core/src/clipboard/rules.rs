//! 用户文本规则：有界编译、按设置快照共享缓存，以及 UTF-8 区间匹配。

use crate::settings::Language;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{
    ops::Range,
    sync::{Arc, OnceLock},
};

pub const MAX_CUSTOM_RULES: usize = 50;
pub const MAX_PATTERN_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RuleAction {
    #[default]
    Snippet,
    Sensitive,
    Ignore,
}

/// id、名称、表达式和动作必须可读；只有 enabled 缺省时自动启用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustomRule {
    pub id: String,
    pub name: String,
    pub pattern: String,
    pub action: RuleAction,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleError {
    TooMany,
    Identity,
    Name,
    TooLong,
    Invalid,
    Empty,
}

impl RuleError {
    pub fn message(self, language: Language) -> &'static str {
        crate::i18n::custom_rules::message(language, self)
    }
}

const SIZE_LIMIT: usize = 1024 * 1024;

/// `regex` 能编译的表达式照旧走线性引擎；只有它拒绝的断言、反向引用、`\K` 才交给 fancy-regex。
/// fancy-regex 不接受 `(?-u:…)`，模板和存量规则都在用，所以不能整体换掉。
#[derive(Debug, Clone)]
pub enum RulePattern {
    Plain(Regex),
    Fancy(fancy_regex::Regex),
}
impl RulePattern {
    /// 回溯超限等运行期错误按未命中处理。
    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Plain(regex) => regex.is_match(text),
            Self::Fancy(regex) => regex.is_match(text).unwrap_or(false),
        }
    }

    /// 非空匹配区间；fancy 遇到运行期错误时停止迭代。
    pub fn find_iter<'a>(&'a self, text: &'a str) -> Box<dyn Iterator<Item = Range<usize>> + 'a> {
        let ranges: Box<dyn Iterator<Item = Range<usize>> + 'a> = match self {
            Self::Plain(regex) => Box::new(regex.find_iter(text).map(|found| found.range())),
            Self::Fancy(regex) => Box::new(
                regex
                    .find_iter(text)
                    .map_while(Result::ok)
                    .map(|found| found.range()),
            ),
        };
        Box::new(ranges.filter(|range| !range.is_empty()))
    }
}

/// 限制源码长度与编译内存；禁止空串规则命中所有剪贴板。
pub fn compile_pattern(pattern: &str) -> Result<RulePattern, RuleError> {
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(RuleError::TooLong);
    }
    let compiled = match RegexBuilder::new(pattern).size_limit(SIZE_LIMIT).build() {
        Ok(regex) => RulePattern::Plain(regex),
        Err(_) => fancy_regex::RegexBuilder::new(pattern)
            .delegate_size_limit(SIZE_LIMIT)
            // 默认 100 万次回溯在 4 MB 文本上连普通后行断言都会超限。
            .backtrack_limit(10_000_000)
            .build()
            .map(RulePattern::Fancy)
            .map_err(|_| RuleError::Invalid)?,
    };
    let empty = match &compiled {
        RulePattern::Plain(regex) => regex.is_match(""),
        RulePattern::Fancy(regex) => regex.is_match("").unwrap_or(true),
    };
    if empty {
        return Err(RuleError::Empty);
    }
    Ok(compiled)
}

/// 写盘前校验整份规则，包含停用规则。
pub fn validate_rules(rules: &[CustomRule]) -> Result<(), RuleError> {
    if rules.len() > MAX_CUSTOM_RULES {
        return Err(RuleError::TooMany);
    }
    let mut ids = std::collections::HashSet::new();
    for rule in rules {
        if rule.id.trim().is_empty() || !ids.insert(&rule.id) {
            return Err(RuleError::Identity);
        }
        if rule.name.trim().is_empty() {
            return Err(RuleError::Name);
        }
        compile_pattern(&rule.pattern)?;
    }
    Ok(())
}

/// 缓存不属于设置数据；克隆快照共享一次初始化，新修订得到新缓存。
#[derive(Debug, Clone, Default)]
pub struct RuleCache(Arc<OnceLock<CompiledRules>>);
impl PartialEq for RuleCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for RuleCache {}
impl RuleCache {
    pub fn get<'a>(&'a self, rules: &[CustomRule]) -> &'a CompiledRules {
        self.0.get_or_init(|| CompiledRules::new(rules))
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompiledRules {
    rules: Vec<(CustomRule, RulePattern)>,
}
impl CompiledRules {
    /// 读盘损坏的表达式不参与执行，不影响其余规则。
    pub fn new(rules: &[CustomRule]) -> Self {
        Self {
            rules: rules
                .iter()
                .take(MAX_CUSTOM_RULES)
                .filter(|rule| rule.enabled)
                .filter_map(|rule| match compile_pattern(&rule.pattern) {
                    Ok(regex) => Some((rule.clone(), regex)),
                    Err(error) => {
                        log::warn!("invalid custom rule {:?}: {error:?}", rule.id);
                        None
                    }
                })
                .collect(),
        }
    }

    pub fn matches(&self, action: RuleAction, text: &str) -> bool {
        self.rules
            .iter()
            .any(|(rule, regex)| rule.action == action && regex.is_match(text))
    }

    /// 按规则顺序返回非空匹配；调用者负责重叠优先级和展示上限。
    pub fn spans<'a>(
        &'a self,
        action: RuleAction,
        text: &'a str,
    ) -> impl Iterator<Item = (Range<usize>, &'a str)> + 'a {
        self.rules
            .iter()
            .filter(move |(rule, _)| rule.action == action)
            .flat_map(move |(rule, regex)| {
                regex
                    .find_iter(text)
                    .map(move |range| (range, rule.name.as_str()))
            })
    }
}

/// ASCII 词边界可紧贴中文；身份证模板校验日期格式及 X 末位，不计算加权校验和。
pub const TEMPLATES: [(&str, RuleAction, &str); 5] = [
    (
        "tracking",
        RuleAction::Snippet,
        r"(?-u:\b)(?:SF|YT|JT|JDV?)[0-9]{10,15}(?-u:\b)",
    ),
    (
        "identity",
        RuleAction::Sensitive,
        r"(?-u:\b)[1-9][0-9]{5}(?:(?:19|20)[0-9]{2}(?:(?:0[13578]|1[02])(?:0[1-9]|[12][0-9]|3[01])|(?:0[469]|11)(?:0[1-9]|[12][0-9]|30)|02(?:0[1-9]|1[0-9]|2[0-8]))|(?:19(?:0[48]|[2468][048]|[13579][26])|20(?:00|0[48]|[2468][048]|[13579][26]))0229)[0-9]{3}[0-9Xx](?-u:\b)",
    ),
    (
        "ipv4",
        RuleAction::Snippet,
        r"(?-u:\b)(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?-u:\b)",
    ),
    (
        "mac",
        RuleAction::Snippet,
        r"(?-u:\b)(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}(?-u:\b)|(?-u:\b)(?:[0-9A-Fa-f]{2}-){5}[0-9A-Fa-f]{2}(?-u:\b)",
    ),
    (
        "ticket",
        RuleAction::Snippet,
        r"(?-u:\b)[A-Z]{2,5}-[0-9]{4,8}(?-u:\b)",
    ),
];

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn rule(pattern: &str, action: RuleAction) -> CustomRule {
        CustomRule {
            id: "r".into(),
            name: "Example".into(),
            pattern: pattern.into(),
            action,
            enabled: true,
        }
    }
    #[test]
    fn validation_rejects_bad_patterns_names_ids_and_limits() {
        assert_eq!(compile_pattern("[").unwrap_err(), RuleError::Invalid);
        assert_eq!(compile_pattern("a*").unwrap_err(), RuleError::Empty);
        assert_eq!(
            compile_pattern(&"a".repeat(501)).unwrap_err(),
            RuleError::TooLong
        );
        assert_eq!(
            compile_pattern("(?:a{1000}){1000}").unwrap_err(),
            RuleError::Invalid
        );
        assert!(compile_pattern(&"中".repeat(500)).is_ok());
        let mut value = rule("abc", RuleAction::Snippet);
        value.name = "  ".into();
        assert_eq!(validate_rules(&[value]), Err(RuleError::Name));
        let value = rule("abc", RuleAction::Snippet);
        assert_eq!(
            validate_rules(&[value.clone(), value.clone()]),
            Err(RuleError::Identity)
        );
        assert_eq!(validate_rules(&vec![value; 51]), Err(RuleError::TooMany));
    }
    fn first<'a>(regex: &RulePattern, text: &'a str) -> Option<&'a str> {
        regex.find_iter(text).next().map(|range| &text[range])
    }
    #[test]
    fn advanced_syntax_compiles_and_matches() {
        let keep = compile_pattern(r"(?i)(?:https?://)?\K[^/:?#]+").unwrap();
        assert_eq!(
            first(&keep, "https://www.example.com/path"),
            Some("www.example.com")
        );
        let behind = compile_pattern(r"(?<=单号)[0-9]+").unwrap();
        assert_eq!(first(&behind, "单号123456已发出"), Some("123456"));
        let backref = compile_pattern(r"(\w+) \1").unwrap();
        assert!(backref.is_match("hello hello"));
        assert!(!backref.is_match("hello world"));
        assert_eq!(compile_pattern(r"(a*)\1").unwrap_err(), RuleError::Empty);
    }
    #[test]
    fn disabled_rules_do_not_execute() {
        let mut value = rule("abc", RuleAction::Ignore);
        value.enabled = false;
        let rules = CompiledRules::new(&[value]);
        assert!(!rules.matches(RuleAction::Ignore, "abc"));
    }
    #[test]
    fn templates_match_ascii_and_chinese_neighbors() {
        let samples = [
            "SF1234567890123",
            "110101199003071234",
            "192.168.1.1",
            "AA:BB:CC:DD:EE:FF",
            "ABC-12345",
        ];
        for ((_, _, pattern), sample) in TEMPLATES.iter().zip(samples) {
            let regex = compile_pattern(pattern).unwrap();
            assert!(matches!(regex, RulePattern::Plain(_)));
            assert_eq!(first(&regex, sample), Some(sample));
            let chinese = format!("单号{sample}已发出");
            assert_eq!(first(&regex, &chinese), Some(sample));
            assert!(!regex.is_match(&format!("A{sample}Z")));
        }
        let tracking = compile_pattern(TEMPLATES[0].2).unwrap();
        for prefix in ["SF", "YT", "JT", "JD", "JDV"] {
            assert!(tracking.is_match(&format!("单号{prefix}1234567890123已发出")));
        }
        let identity = compile_pattern(TEMPLATES[1].2).unwrap();
        assert!(identity.is_match("身份证110101199003071234号"));
        assert!(identity.is_match("身份证11010120000229123X号"));
        for bad in [
            "110101199002291234",
            "110101199013071234",
            "110101199004311234",
            "110101190002291234",
        ] {
            assert!(!identity.is_match(bad), "{bad}");
        }
        assert!(!compile_pattern(TEMPLATES[2].2)
            .unwrap()
            .is_match("256.256.256.256"));
        assert!(compile_pattern(TEMPLATES[3].2)
            .unwrap()
            .is_match("地址aa-bb-cc-dd-ee-ff结束"));
    }
    #[test]
    fn revision_snapshots_share_compiled_rules_but_new_revision_rebuilds() {
        let clipboard = crate::settings::Clipboard {
            custom_rules: vec![rule("abc", RuleAction::Ignore)],
            ..Default::default()
        };
        let snapshot = clipboard.clone();
        assert!(std::ptr::eq(
            clipboard.compiled_rules(),
            snapshot.compiled_rules()
        ));
        let json = serde_json::to_value(&clipboard).unwrap();
        assert!(json.get("ruleCache").is_none());
        let next: crate::settings::Clipboard = serde_json::from_value(json).unwrap();
        assert!(!std::ptr::eq(
            clipboard.compiled_rules(),
            next.compiled_rules()
        ));
    }
}
