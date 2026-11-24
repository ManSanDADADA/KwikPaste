//! 起真实的扩展进程走一帧。

use std::io::Cursor;
use std::process::{Command, Stdio};

use kwikpaste_ext_protocol::{
    TranslateEngine, TranslateOutcome, TranslateRequest, TranslateResponse, read_frame, write_frame,
};

/// 发一帧请求、关掉 stdin，读回这一帧的结果；进程应当正常退出。
fn exchange(text: &str, auto: bool) -> TranslateOutcome {
    let request = TranslateRequest::Translate {
        job_id: "job".into(),
        text: text.into(),
        first: "zh-Hans".into(),
        second: "en".into(),
        auto,
        engine: TranslateEngine::Free { service: None },
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_kwikpaste-ext-translate"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut buffer = Vec::new();
    let mut stdin = child.stdin.take().unwrap();
    write_frame(&mut stdin, &request, &mut buffer).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let TranslateResponse::Translate { job_id, outcome } =
        read_frame::<TranslateResponse>(&mut Cursor::new(output.stdout), &mut buffer)
            .unwrap()
            .unwrap();
    assert_eq!(job_id, "job");
    outcome
}

#[test]
fn auto_mode_skips_the_first_language_without_network() {
    let outcome = exchange(
        "剪贴板管理器可以保存你复制过的内容，方便以后再找出来。",
        true,
    );
    assert_eq!(outcome, TranslateOutcome::Skipped);
}

/// 发布前探测火山接口还能不能用：`cargo test -p kwikpaste-ext-translate -- --ignored`。
/// `provider` 不是 `volc` 说明火山坏了、腾讯在顶替。
#[test]
#[ignore = "calls the real translation service"]
fn live_free_translation() {
    let outcome = exchange("The clipboard manager keeps everything you copy.", false);
    let TranslateOutcome::Done {
        text,
        source,
        target,
        provider,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert!(
        text.chars()
            .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
        "{text}"
    );
    assert_eq!(
        (source.as_deref(), target.as_str(), provider.as_str()),
        (Some("en"), "zh-Hans", "volc")
    );
}
