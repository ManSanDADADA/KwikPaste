//! 免费翻译：火山翻译和腾讯交互翻译的网页接口。都是非官方接口，按顺序尝试。

use std::time::Duration;

use kwikpaste_ext_protocol::TranslateFailure;
use reqwest::blocking::Client;
use serde_json::{Value, json};

use crate::{Failure, Translated, brief, network};

const TIMEOUT: Duration = Duration::from_secs(10);

/// 依次尝试各家；全部失败时返回最后一家的失败。
pub fn translate(client: &Client, text: &str, target: &str) -> Result<Translated, Failure> {
    volc(client, text, target).or_else(|failure| {
        eprintln!("volc failed: {}", failure.message);
        transmart(client, text, target)
    })
}

/// 发 JSON、取响应正文。429 算限流，其它非 2xx 算其它。
fn post(client: &Client, url: &str, body: &Value) -> Result<String, Failure> {
    let response = client
        .post(url)
        .timeout(TIMEOUT)
        .json(body)
        .send()
        .map_err(network)?;
    let status = response.status();
    let text = response.text().map_err(network)?;
    if status.as_u16() == 429 {
        return Err(Failure {
            kind: TranslateFailure::RateLimited,
            message: brief(&text),
        });
    }
    if !status.is_success() {
        return Err(Failure {
            kind: TranslateFailure::Other,
            message: format!("HTTP {}: {}", status.as_u16(), brief(&text)),
        });
    }
    Ok(text)
}

fn other(body: &str) -> Failure {
    Failure {
        kind: TranslateFailure::Other,
        message: brief(body),
    }
}

fn volc(client: &Client, text: &str, target: &str) -> Result<Translated, Failure> {
    // 火山的简体中文是 `zh`，其余 10 种与 BCP 47 相同。
    let target = if target == "zh-Hans" { "zh" } else { target };
    let body = json!({"source_language": "", "target_language": target, "text": text});
    let response = post(
        client,
        "https://translate.volcengine.com/crx/translate/v1/",
        &body,
    )?;
    parse_volc(&response)
}

fn parse_volc(body: &str) -> Result<Translated, Failure> {
    let value: Value = serde_json::from_str(body).map_err(|_| other(body))?;
    let text = value["translation"].as_str().unwrap_or_default();
    if text.is_empty() {
        return Err(other(body));
    }
    Ok(Translated {
        text: text.to_owned(),
        source: value["detected_language"]
            .as_str()
            .filter(|code| *code != "not_lang")
            .map(str::to_owned),
        provider: "volc".to_owned(),
    })
}

fn transmart(client: &Client, text: &str, target: &str) -> Result<Translated, Failure> {
    let target = match target {
        "zh-Hans" => "zh",
        "zh-Hant" => "zh-TW",
        other => other,
    };
    let body = json!({
        "header": {
            "fn": "auto_translation",
            "client_key": "browser-chrome-130.0.0-Windows_10-kwikpaste",
        },
        "type": "plain",
        "model_category": "normal",
        "source": {"lang": "auto", "text_list": [text]},
        "target": {"lang": target},
    });
    let response = post(client, "https://transmart.qq.com/api/imt", &body)?;
    parse_transmart(&response, target)
}

/// `target` 是请求里发出的腾讯语种代码。
fn parse_transmart(body: &str, target: &str) -> Result<Translated, Failure> {
    let value: Value = serde_json::from_str(body).map_err(|_| other(body))?;
    if value["header"]["ret_code"].as_str() != Some("succ") {
        let message = value["message"].as_str().unwrap_or(body);
        let kind = if message.contains("language") {
            TranslateFailure::Unsupported
        } else {
            TranslateFailure::Other
        };
        return Err(Failure {
            kind,
            message: brief(message),
        });
    }
    let text = value["auto_translation"][0].as_str().unwrap_or_default();
    if text.is_empty() {
        return Err(other(body));
    }
    Ok(Translated {
        text: text.to_owned(),
        // 原文已是目标语种时，腾讯不返回 `src_lang`（也不返回 `tgt_lang`）。
        source: Some(value["src_lang"].as_str().unwrap_or(target).to_owned()),
        provider: "transmart".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 以下正文都是 2026-11-17 实测录下来的。
    const VOLC_OK: &str = r#"{"translation":"第一行。\n第二行。","detected_language":"en","probability":1,"base_resp":{"status_code":0,"status_message":""}}"#;
    const VOLC_EMPTY: &str = r#"{"translation":"","detected_language":"not_lang","probability":1,"base_resp":{"status_code":0,"status_message":""}}"#;
    const TRANSMART_OK: &str = r#"{"header":{"type":"auto_translation","ret_code":"succ","time_cost":230.0,"request_id":"c177a354e28711f1ba770f149b5e1739"},"auto_translation":["剪贴板管理器保存您复制的所有内容。"],"src_lang":"en","tgt_lang":"zh"}"#;
    const TRANSMART_ERROR: &str = r#"{"header":{"type":"auto_translation","ret_code":"error","time_cost":0.0,"request_id":"c3378b6fe28711f1824d20d2eac3cd5a"},"message":"Unrecognized target language: xx"}"#;
    // 2026-11-18 实测：原文已是目标语种（en → en）时没有 `src_lang`。
    const TRANSMART_SAME: &str = r#"{"header":{"type":"auto_translation","ret_code":"succ","time_cost":5.0,"request_id":"c19a910ee32111f19b52d198e8604208"},"auto_translation":["Hello world"]}"#;

    #[test]
    fn parses_volc_responses() {
        let done = parse_volc(VOLC_OK).unwrap();
        assert_eq!(done.text, "第一行。\n第二行。");
        assert_eq!(done.source.as_deref(), Some("en"));
        assert_eq!(done.provider, "volc");
        // 空译文算失败，好让下一家接手；纯文本的 400 正文也是。
        assert_eq!(
            parse_volc(VOLC_EMPTY).unwrap_err().kind,
            TranslateFailure::Other
        );
        assert_eq!(
            parse_volc("Bad Request").unwrap_err().kind,
            TranslateFailure::Other
        );
    }

    #[test]
    fn parses_transmart_responses() {
        let done = parse_transmart(TRANSMART_OK, "zh").unwrap();
        assert_eq!(done.text, "剪贴板管理器保存您复制的所有内容。");
        assert_eq!(done.source.as_deref(), Some("en"));
        assert_eq!(done.provider, "transmart");
        let same = parse_transmart(TRANSMART_SAME, "en").unwrap();
        assert_eq!(same.source.as_deref(), Some("en"));
        let failure = parse_transmart(TRANSMART_ERROR, "xx").unwrap_err();
        assert_eq!(failure.kind, TranslateFailure::Unsupported);
        assert_eq!(failure.message, "Unrecognized target language: xx");
    }

    /// 发布前探测：`cargo test -p kwikpaste-ext-translate -- --ignored`。
    #[test]
    #[ignore = "calls the real translation service"]
    fn live_transmart() {
        let done = transmart(
            &Client::new(),
            "The clipboard manager keeps everything you copy.",
            "zh-Hans",
        )
        .unwrap();
        assert_eq!(done.source.as_deref(), Some("en"));
        assert!(!done.text.is_empty());
    }
}
