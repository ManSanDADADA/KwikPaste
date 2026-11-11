//! 高置信密钥 / token 检测。
//!
//! 规则刻意保守：只拦截带固定前缀、标准结构或明确私钥边界的内容，
//! 避免把普通长文本、颜色值、文件路径等误判为敏感凭据。

use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;

/// 判断文本是否包含高置信 secret/token，命中时整条剪贴板内容按敏感处理。
pub fn contains_secret(text: &str) -> bool {
    secret_spans(text).next().is_some()
}

/// 内置规则命中的字节区间（可能重叠、无序）；带字段名的赋值只给出值的部分。
pub fn secret_spans(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    [
        private_key_block(),
        known_prefixed_token(),
        aws_access_key(),
        jwt(),
    ]
    .into_iter()
    .flat_map(move |regex| regex.find_iter(text).map(|found| found.range()))
    .chain(
        labeled_secret()
            .captures_iter(text)
            .filter_map(|captures| captures.get(1).map(|value| value.range())),
    )
}

/// 敏感条目里 curl / fetch / PowerShell 命令与配置片段中的凭据值：`Authorization` / `Cookie` / `x-*-token`
/// 请求头、`-u user:pass`、URL 里的 `user:pass@`、`password` 类赋值。只用于遮罩：这些写法本身不让条目变敏感，
/// 但条目因别的凭据变敏感后，同一段命令里的这些值也得遮住，其余文字保留以便辨认。
pub fn credential_header_spans(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    static HEADER_RE: OnceLock<Regex> = OnceLock::new();
    static USER_RE: OnceLock<Regex> = OnceLock::new();
    static USERINFO_RE: OnceLock<Regex> = OnceLock::new();
    static PASSWORD_RE: OnceLock<Regex> = OnceLock::new();

    // 请求头名后可跟闭合引号再接 `:` 或 `=`（JSON 的 "cookie": "..."、PowerShell 的 "authorization"="..."）；
    // 不带引号时只认 `:`，免得把普通文字里的 `cookie=` 当请求头。
    let header = HEADER_RE.get_or_init(|| {
        Regex::new(
            r#"(?mx)
            (?:
                (?:^|[\s'"])(?i:authorization|cookie|x-api-key|x-[a-z0-9-]*token)(?:['"]\s*[:=]|\s*:)\s*['"]?
              | (?:^|\s)(?:-b|--cookie)\s+\^?['"]
            )
            ([^'"\r\n]+)
            "#,
        )
        .expect("valid regex")
    });
    // curl `-u user:pass` / `--user user:pass`，只遮密码。
    let user = USER_RE.get_or_init(|| {
        Regex::new(r#"(?:^|\s)(?:-u|--user)(?:\s+|=)['"]?[^\s'":]+:([^\s'"]+)"#)
            .expect("valid regex")
    });
    // URL 里的 `scheme://user:pass@host`，只遮密码。
    let userinfo = USERINFO_RE.get_or_init(|| {
        Regex::new(r#"(?i)[a-z][a-z0-9+.-]*://[^\s/:@'"]+:([^\s/@'"]+)@"#).expect("valid regex")
    });
    // `password` / `passwd` / `pwd` 赋值：JSON、表单、`.env` 的 `DB_PASSWORD=`，以及被转义的引号。
    let password = PASSWORD_RE.get_or_init(|| {
        Regex::new(r#"(?i)(?:password|passwd|pwd)\\?['"]?\s*[:=]\s*\\?['"]?([^\s'",;&\\]+)"#)
            .expect("valid regex")
    });

    [header, user, userinfo, password]
        .into_iter()
        .flat_map(move |regex| {
            regex
                .captures_iter(text)
                .filter_map(|captures| captures.get(1).map(|value| value.range()))
        })
}

/// 识别 PEM/OpenSSH 私钥块，区间延伸到 END 行（缺失时到文本末尾），避免露出私钥正文。
fn private_key_block() -> &'static Regex {
    static PRIVATE_KEY_RE: OnceLock<Regex> = OnceLock::new();
    PRIVATE_KEY_RE.get_or_init(|| {
        Regex::new(
            r"(?s)-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----.*?(?:-----END (?:[A-Z0-9 ]+ )?PRIVATE KEY-----|\z)",
        )
        .expect("valid regex")
    })
}

/// 识别常见服务商自带前缀的 token。
fn known_prefixed_token() -> &'static Regex {
    static PREFIXED_TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    PREFIXED_TOKEN_RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            (?i:
                (?-u:\b)gh[pousr]_[A-Za-z0-9_]{36,}(?-u:\b)
              | (?-u:\b)github_pat_[A-Za-z0-9_]{40,}(?-u:\b)
              | (?-u:\b)sk-[A-Za-z0-9_-]{32,}(?-u:\b)
              | (?-u:\b)sk-proj-[A-Za-z0-9_-]{32,}(?-u:\b)
              | (?-u:\b)xox[baprs]-[A-Za-z0-9-]{20,}(?-u:\b)
              | (?-u:\b)(?:api|access|refresh|secret)[_-]?token_[A-Za-z0-9_-]{24,}(?-u:\b)
            )
            ",
        )
        .expect("valid regex")
    })
}

/// 识别 AWS access key id。Secret access key 无固定前缀，单独识别会误伤，暂不拦截。
fn aws_access_key() -> &'static Regex {
    static AWS_KEY_RE: OnceLock<Regex> = OnceLock::new();
    AWS_KEY_RE.get_or_init(|| {
        Regex::new(r"(?-u:\b)(?:AKIA|ASIA)[A-Z0-9]{16}(?-u:\b)").expect("valid regex")
    })
}

/// 识别结构完整的 JWT：三段 base64url，header 解码后包含 typ/alg 常见字段。
fn jwt() -> &'static Regex {
    static JWT_RE: OnceLock<Regex> = OnceLock::new();
    JWT_RE.get_or_init(|| {
        Regex::new(r"(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?-u:\b)")
            .expect("valid regex")
    })
}

/// 识别带明确字段名的 secret 赋值，第 1 组是值。字段名与分隔符之间允许一个可选的闭合引号，
/// 以覆盖 JSON / 引号包裹的配置（`"api_key": "..."`、`'secret_key': '...'`）。
fn labeled_secret() -> &'static Regex {
    static LABELED_SECRET_RE: OnceLock<Regex> = OnceLock::new();
    LABELED_SECRET_RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            (?-u:\b)
            (?:api[_-]?key|secret[_-]?key|client[_-]?secret|access[_-]?token|refresh[_-]?token|auth[_-]?token|bearer)
            (?-u:\b)
            ['"]?\s*[:=]\s*
            ['"]?
            ([A-Za-z0-9][A-Za-z0-9._~+/=-]{23,})
            "#,
        )
        .expect("valid regex")
    })
}

#[cfg(test)]
mod tests {
    use super::{contains_secret, credential_header_spans, secret_spans};

    #[test]
    fn spans_cover_labeled_values_and_whole_private_keys() {
        let labeled = "api_key = abcdefghijklmnopqrstuvwxyz123456\nnext";
        let spans: Vec<_> = secret_spans(labeled).map(|span| &labeled[span]).collect();
        assert_eq!(spans, ["abcdefghijklmnopqrstuvwxyz123456"]);

        let private_key = [
            "-----BEGIN ",
            "OPENSSH PRIVATE KEY",
            "-----\nabc\n-----END OPENSSH PRIVATE KEY-----\nafter",
        ]
        .concat();
        let spans: Vec<_> = secret_spans(&private_key)
            .map(|span| &private_key[span])
            .collect();
        assert_eq!(spans, [private_key.trim_end_matches("\nafter")]);
    }

    #[test]
    fn detects_known_prefixed_tokens() {
        let github_token = ["ghp", "_abcdefghijklmnopqrstuvwxyzABCDE1234567890"].concat();
        let openai_token = ["sk-proj", "-abcdefghijklmnopqrstuvwxyzABCDE1234567890"].concat();
        let slack_token = ["xoxb", "-123456789012-abcdefABCDEFabcdefABCDEF"].concat();

        assert!(contains_secret(&github_token));
        assert!(contains_secret(&openai_token));
        assert!(contains_secret(&slack_token));
    }

    #[test]
    fn detects_ascii_credentials_next_to_cjk_but_not_inside_ascii_identifiers() {
        let credentials = [
            ["ghp", "_abcdefghijklmnopqrstuvwxyzABCDE1234567890"].concat(),
            ["AKIA", "IOSFODNN7EXAMPLE"].concat(),
            "eyJhbGciOiJIUzI1NiJ9.abcdefghijk.abcdefghijk".to_owned(),
            "api_key=abcdefghijklmnopqrstuvwxyz123456".to_owned(),
        ];

        for credential in credentials {
            assert!(contains_secret(&format!("密钥{credential}结束")));
            assert!(!contains_secret(&format!("prefix_{credential}")));
        }
    }

    #[test]
    fn detects_private_keys_aws_keys_and_jwt() {
        let aws_key = ["AKIA", "IOSFODNN7EXAMPLE"].concat();
        let private_key = [
            "-----BEGIN ",
            "OPENSSH PRIVATE KEY",
            "-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
        ]
        .concat();
        let jwt = [
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            ".SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        ]
        .concat();

        assert!(contains_secret(&private_key));
        assert!(contains_secret(&aws_key));
        assert!(contains_secret(&jwt));
    }

    #[test]
    fn detects_labeled_secrets() {
        let labeled_secret = ["client", "_secret = abcdefghijklmnopqrstuvwxyz123456"].concat();
        let bearer_token = ["Authorization bearer", ": abcdefghijklmnopqrstuvwxyz123456"].concat();

        assert!(contains_secret(&labeled_secret));
        assert!(contains_secret(&bearer_token));
    }

    #[test]
    fn detects_labeled_secrets_with_quoted_keys() {
        // JSON / 引号包裹的字段名：label 与分隔符之间的闭合引号此前断开了正则，导致漏判。
        // 值用明显的假串（无任何服务商前缀），仅用于驱动 has_labeled_secret 命中。
        let json_double = r#"{"api_key": "dummy_secret_value_abcdefghijklmnopqr"}"#;
        assert!(contains_secret(json_double));

        let single_quoted_key = r#"'secret_key': "abcdefghijklmnopqrstuvwxyz12345678""#;
        assert!(contains_secret(single_quoted_key));

        // 单引号 key + 单引号 value（部分 shell / dotenv 风格）。
        let single_quoted_both = r#"'access_token': 'abcdefghijklmnopqrstuvwxyz123456'"#;
        assert!(contains_secret(single_quoted_both));
    }

    fn header_values(text: &str) -> Vec<&str> {
        credential_header_spans(text)
            .map(|span| &text[span])
            .collect()
    }

    #[test]
    fn masks_credentials_in_curl_fetch_and_powershell_commands() {
        let curl =
            "curl -H 'Cookie: sid=abc' -H 'x-auth-token: tok1' -u admin:pw1 https://u:pw2@host/x";
        assert_eq!(header_values(curl), ["sid=abc", "tok1", "pw1", "pw2"]);
        assert_eq!(header_values("curl --user=admin:pw1 host"), ["pw1"]);

        let fetch =
            r#"fetch(u, {"headers": {"authorization": "Bearer abc.def", "cookie": "sid=1; b=2"}})"#;
        assert_eq!(header_values(fetch), ["Bearer abc.def", "sid=1; b=2"]);

        let powershell = r#"$h = @{ "authorization"="Bearer abc"; "x-csrf-token"="t9" }"#;
        assert_eq!(header_values(powershell), ["Bearer abc", "t9"]);

        let body = r#"--data-raw '{"password":"p@ss1","user":"a"}' && DB_PASSWORD=p4ss2
pwd=p3&x=1"#;
        assert_eq!(header_values(body), ["p@ss1", "p4ss2", "p3"]);
        let escaped = r#"--data-raw "{\"password\":\"p@ss1\"}""#;
        assert_eq!(header_values(escaped), ["p@ss1"]);
    }

    #[test]
    fn credential_spans_ignore_prose_and_plain_urls() {
        assert!(
            header_values("Reset your password in settings, cookie policy, pwd is cwd").is_empty()
        );
        assert!(header_values("https://example.com:8080/a@b curl -u admin host").is_empty());
    }

    #[test]
    fn ignores_ordinary_text_and_short_codes() {
        assert!(!contains_secret(
            "这是一个普通剪贴板文本，包含 token 这个单词。"
        ));
        assert!(!contains_secret("验证码 123456"));
        assert!(!contains_secret("https://example.com/path/to/resource"));
        assert!(!contains_secret(
            "AKIA is just a word without enough characters"
        ));
    }
}
