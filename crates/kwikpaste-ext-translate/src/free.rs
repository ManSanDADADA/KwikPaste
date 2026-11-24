//! 免费翻译：谷歌、微软（必应）、火山、腾讯交互翻译的网页接口，都是非官方接口。
//! 用户指定了哪家就只用哪家；没指定时依次尝试火山、腾讯、微软。谷歌在国内要代理才连得上，不放进默认顺序。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use kwikpaste_ext_protocol::{FreeService, TranslateFailure};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{Value, json};

use crate::{Failure, Translated, brief, network};

const TIMEOUT: Duration = Duration::from_secs(10);

pub fn translate(
    client: &Client,
    service: Option<FreeService>,
    text: &str,
    target: &str,
) -> Result<Translated, Failure> {
    match service {
        Some(FreeService::Google) => google(client, text, target),
        Some(FreeService::Microsoft) => microsoft(client, text, target),
        Some(FreeService::Volc) => volc(client, text, target),
        Some(FreeService::Transmart) => transmart(client, text, target),
        None => auto(client, text, target),
    }
}

/// 依次尝试各家；全部失败时返回最后一家的失败。
fn auto(client: &Client, text: &str, target: &str) -> Result<Translated, Failure> {
    volc(client, text, target)
        .or_else(|failure| {
            eprintln!("volc failed: {}", failure.message);
            transmart(client, text, target)
        })
        .or_else(|failure| {
            eprintln!("transmart failed: {}", failure.message);
            microsoft(client, text, target)
        })
}

/// 发 JSON、取响应正文。
fn post(client: &Client, url: &str, body: &Value) -> Result<String, Failure> {
    send(client.post(url).json(body))
}

/// 发请求、取响应正文。429 算限流，其它非 2xx 算其它。
fn send(request: RequestBuilder) -> Result<String, Failure> {
    let response = request.timeout(TIMEOUT).send().map_err(network)?;
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

fn google(client: &Client, text: &str, target: &str) -> Result<Translated, Failure> {
    let target = match target {
        "zh-Hans" => "zh-CN",
        "zh-Hant" => "zh-TW",
        other => other,
    };
    let request = client
        .post("https://clients5.google.com/translate_a/t")
        .query(&[("client", "dict-chrome-ex"), ("sl", "auto"), ("tl", target)])
        .form(&[("q", text)]);
    parse_google(&send(request)?)
}

/// `sl=auto` 时正文是 `[["译文","原文语种"]]`。
fn parse_google(body: &str) -> Result<Translated, Failure> {
    let value: Value = serde_json::from_str(body).map_err(|_| other(body))?;
    let text = value[0][0].as_str().unwrap_or_default();
    if text.is_empty() {
        return Err(other(body));
    }
    Ok(Translated {
        text: text.to_owned(),
        source: value[0][1].as_str().map(str::to_owned),
        provider: "google".to_owned(),
    })
}

/// 必应翻译网页的会话参数，页面里给出有效期（一小时）。
struct BingSession {
    /// 跳转后的站点：国内是 `cn.bing.com`。
    origin: String,
    ig: String,
    iid: String,
    key: String,
    token: String,
    expires: Instant,
}

static BING: Mutex<Option<BingSession>> = Mutex::new(None);

fn microsoft(client: &Client, text: &str, target: &str) -> Result<Translated, Failure> {
    let mut cached = BING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = match cached.take() {
        Some(session) if session.expires > Instant::now() => session,
        _ => bing_session(client)?,
    };
    let url = format!(
        "{}/ttranslatev3?isVertical=1&IG={}&IID={}.1",
        session.origin, session.ig, session.iid
    );
    let request = client.post(url).form(&[
        ("fromLang", "auto-detect"),
        ("to", target),
        ("text", text),
        ("token", session.token.as_str()),
        ("key", session.key.as_str()),
    ]);
    let result = send(request).and_then(|body| parse_bing(&body));
    *cached = Some(session);
    result
}

/// 打开翻译页，取出 `IG`、`data-iid` 和 `params_AbusePreventionHelper = [key,"token",有效毫秒]`。
fn bing_session(client: &Client) -> Result<BingSession, Failure> {
    let response = client
        .get("https://www.bing.com/translator")
        .timeout(TIMEOUT)
        .send()
        .map_err(network)?;
    let url = response.url();
    let origin = format!(
        "{}://{}",
        url.scheme(),
        url.host_str().unwrap_or("www.bing.com")
    );
    let page = response.text().map_err(network)?;
    let parsed = parse_bing_page(&page, origin);
    parsed.ok_or_else(|| Failure {
        kind: TranslateFailure::Other,
        message: "Bing Translator page changed".to_owned(),
    })
}

fn parse_bing_page(page: &str, origin: String) -> Option<BingSession> {
    let between = |start: &str, end: &str| -> Option<&str> {
        let rest = &page[page.find(start)? + start.len()..];
        Some(&rest[..rest.find(end)?])
    };
    let ig = between("IG:\"", "\"")?.to_owned();
    let iid = format!("translator{}", between("data-iid=\"translator", "\"")?);
    let helper = between("params_AbusePreventionHelper = [", "]")?;
    let mut parts = helper.splitn(3, ',');
    let key = parts.next()?.trim().to_owned();
    let token = parts.next()?.trim().trim_matches('"').to_owned();
    let ttl: u64 = parts.next()?.trim().parse().ok()?;
    Some(BingSession {
        origin,
        ig,
        iid,
        key,
        token,
        // 留一分钟余量，免得翻到一半过期。
        expires: Instant::now()
            + Duration::from_millis(ttl).saturating_sub(Duration::from_secs(60)),
    })
}

/// 成功是 `[{"detectedLanguage":{"language":"en"},"translations":[{"text":…}]}]`，
/// 失败是 `{"statusCode":400,"errorMessage":…}` 之类的对象。
fn parse_bing(body: &str) -> Result<Translated, Failure> {
    let value: Value = serde_json::from_str(body).map_err(|_| other(body))?;
    let text = value[0]["translations"][0]["text"]
        .as_str()
        .unwrap_or_default();
    if text.is_empty() {
        return Err(other(body));
    }
    Ok(Translated {
        text: text.to_owned(),
        source: value[0]["detectedLanguage"]["language"]
            .as_str()
            .map(str::to_owned),
        provider: "microsoft".to_owned(),
    })
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

    // 2026-11-24 实测。
    const GOOGLE_OK: &str = r#"[["第一行。它有<b>标签</b>和&符号。\n第二行在这里。","en"]]"#;
    const BING_OK: &str = r#"[{"translations":[{"text":"你好，世界。你好吗？","to":"zh-Hans","transliteration":{"text":"Nǐ hǎo, shìjiè. Nǐ hǎo ma?","script":"Latn"}}],"usedLLM":true,"detectedLanguage":{"language":"en"}}]"#;
    const BING_PAGE: &str = r#"<div id="rich_tta" data-iid="translator.5023"></div><script>_G={IG:"32F82020998242DE9BF6FC21A8534309",EF:{}};var params_AbusePreventionHelper = [1795491146726,"w56pFS3ZY_MJS-RProo6lwPg-KL-2IRN",3600000];</script>"#;

    #[test]
    fn parses_google_and_bing_responses() {
        let done = parse_google(GOOGLE_OK).unwrap();
        assert_eq!(
            done.text,
            "第一行。它有<b>标签</b>和&符号。\n第二行在这里。"
        );
        assert_eq!(done.source.as_deref(), Some("en"));
        assert_eq!(done.provider, "google");
        // 被限流时谷歌回一个 HTML 页面。
        assert!(parse_google("<html><title>Sorry...</title></html>").is_err());

        let done = parse_bing(BING_OK).unwrap();
        assert_eq!(done.text, "你好，世界。你好吗？");
        assert_eq!(done.source.as_deref(), Some("en"));
        assert_eq!(done.provider, "microsoft");
        assert!(parse_bing(r#"{"statusCode":205}"#).is_err());

        let session = parse_bing_page(BING_PAGE, "https://cn.bing.com".into()).unwrap();
        assert_eq!(session.ig, "32F82020998242DE9BF6FC21A8534309");
        assert_eq!(session.iid, "translator.5023");
        assert_eq!(session.key, "1795491146726");
        assert_eq!(session.token, "w56pFS3ZY_MJS-RProo6lwPg-KL-2IRN");
        assert!(parse_bing_page("<html></html>", String::new()).is_none());
    }

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

    /// 两次调用：第二次用缓存的会话。
    #[test]
    #[ignore = "calls the real translation service"]
    fn live_microsoft() {
        let client = Client::new();
        for text in [
            "The clipboard manager keeps everything you copy.",
            "Second line.
Third line.",
        ] {
            let done = microsoft(&client, text, "zh-Hans").unwrap();
            assert_eq!(done.source.as_deref(), Some("en"));
            assert!(!done.text.is_empty());
        }
    }

    /// 国内直连连不上，要走代理。
    #[test]
    #[ignore = "calls the real translation service"]
    fn live_google() {
        let done = google(
            &Client::new(),
            "The clipboard manager keeps everything you copy.",
            "zh-Hans",
        )
        .unwrap();
        assert_eq!(done.source.as_deref(), Some("en"));
        assert!(!done.text.is_empty());
    }
}
