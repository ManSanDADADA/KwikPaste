//! 翻译扩展进程：读一帧、翻译、写一帧；stdin 关闭时退出。
mod direction;
mod free;
mod openai;

use std::io::{self, Read, Write};
use std::time::Duration;

use kwikpaste_ext_protocol::{
    self as protocol, TranslateEngine, TranslateFailure, TranslateOutcome, TranslateRequest,
    TranslateResponse,
};
use reqwest::blocking::Client;

use direction::{Plan, plan, same_language};

/// 一次服务调用的结果。`source` 是服务报告的原文语种，可能没有。
#[derive(Debug)]
pub struct Translated {
    pub text: String,
    pub source: Option<String>,
    pub provider: String,
}

/// 服务调用失败：`kind` 给宿主分类显示，`message` 是服务返回的说明或网络错误。
#[derive(Debug)]
pub struct Failure {
    pub kind: TranslateFailure,
    pub message: String,
}

/// 连不上、超时、TLS 失败都算网络错误；地址本身不合法算其它。消息里不带请求地址。
pub(crate) fn network(err: reqwest::Error) -> Failure {
    let kind = if err.is_builder() {
        TranslateFailure::Other
    } else {
        TranslateFailure::Network
    };
    let err = err.without_url();
    // reqwest 的 Display 只有错误类别，具体原因在 source 链上。
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    Failure { kind, message }
}

/// 服务返回的正文截到 300 个字符，给界面显示。
pub(crate) fn brief(body: &str) -> String {
    body.trim().chars().take(300).collect()
}

fn run(mut reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(io::Error::other)?;
    let mut buffer = Vec::new();
    while let Some(request) = protocol::read_frame::<TranslateRequest>(&mut reader, &mut buffer)? {
        let TranslateRequest::Translate {
            job_id,
            text,
            first,
            second,
            auto,
            engine,
        } = request;
        let outcome = translate(&client, &text, &first, &second, auto, &engine);
        let response = TranslateResponse::Translate { job_id, outcome };
        protocol::write_frame(&mut writer, &response, &mut buffer)?;
    }
    Ok(())
}

fn translate(
    client: &Client,
    text: &str,
    first: &str,
    second: &str,
    auto: bool,
    engine: &TranslateEngine,
) -> TranslateOutcome {
    let (mut target, recheck) = match plan(text, first, second, auto) {
        Plan::Skip => return TranslateOutcome::Skipped,
        Plan::Translate { target, recheck } => (target, recheck),
    };
    let call = |target: &str| match engine {
        TranslateEngine::Free { service } => free::translate(client, *service, text, target),
        TranslateEngine::OpenAi {
            base_url,
            api_key,
            model,
            prompt,
        } => openai::translate(
            client,
            base_url,
            api_key,
            model,
            prompt.as_deref(),
            text,
            target,
        ),
    };
    let mut result = call(&target);
    // 语种没拿准时先按首选语言翻；服务说原文就是首选语言，改译成第二语言。AI 接口不报语种，没有这一步。
    let is_first = |done: &Translated| {
        done.source
            .as_deref()
            .is_some_and(|source| same_language(source, first))
    };
    if recheck && result.as_ref().is_ok_and(is_first) {
        target = second.to_owned();
        result = call(&target);
    }
    match result {
        Ok(done) => TranslateOutcome::Done {
            text: done.text,
            source: done.source,
            target,
            provider: done.provider,
        },
        Err(failure) => TranslateOutcome::Failed {
            kind: failure.kind,
            message: failure.message,
        },
    }
}

fn main() -> io::Result<()> {
    run(io::stdin().lock(), io::stdout().lock())
}
