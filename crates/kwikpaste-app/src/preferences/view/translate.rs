//! 翻译那一页的设置：引擎、两种语言、两个开关、AI 接口和清除译文。
//!
//! AI 接口的四项存在 core 的扩展状态文件里（不进 settings.json）：单行输入框失焦或回车时保存，
//! 提示词改了就保存。

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, radians, rems,
};
use kwikpaste_core::settings::{Settings, TranslateEngineKind};
use kwikpaste_core::translate::{self, TranslateAiConfig};
use kwikpaste_ui::{
    Button, DialogSpec, Icon, IconName, Input, KpStyled as _, Select, SelectOption, SelectState,
    TextArea, TextAreaInput, TextInput, form_dialog,
    theme::{self, TextSize, space},
    toast::{self, Toast},
};
use serde_json::json;

use super::{CONTROL_WIDTH, Preferences, row_frame, row_label, setting_row};
use crate::{clipboard::model::translation::language_name, core_host, i18n};

/// AI 接口预设：标识、地址、建议的模型。选了就填好地址和模型，都可以再改。
const PRESETS: [(&str, &str, &str); 5] = [
    ("deepseek", "https://api.deepseek.com", "deepseek-flash"),
    (
        "zhipu",
        "https://open.bigmodel.cn/api/paas/v4",
        "glm-4.7-flash",
    ),
    (
        "bailian",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "qwen-flash",
    ),
    (
        "siliconflow",
        "https://api.siliconflow.cn/v1",
        "tencent/Hunyuan-MT-7B",
    ),
    ("ollama", "http://localhost:11434/v1", "translategemma"),
];

fn key(name: &str) -> SharedString {
    i18n::t(&format!("preferences:extensions.translate.{name}"))
}

fn engine_value(engine: TranslateEngineKind) -> &'static str {
    match engine {
        TranslateEngineKind::Free => "free",
        TranslateEngineKind::Ai => "ai",
    }
}

fn engine_options() -> Vec<SelectOption> {
    Vec::from(
        ["free", "ai"].map(|value| SelectOption::new(value, key(&format!("engines.{value}")))),
    )
}

/// 语言选项。`follow`：最前面加「跟随界面语言」（值是空串）；`exclude`：去掉这一种。
fn language_options(follow: bool, exclude: Option<&str>) -> Vec<SelectOption> {
    let mut options = Vec::new();
    if follow {
        options.push(SelectOption::new("", key("followInterface")));
    }
    options.extend(
        translate::LANGUAGES
            .iter()
            .copied()
            .filter(|code| Some(*code) != exclude)
            .map(|code| SelectOption::new(code, language_name(code))),
    );
    options
}

/// 预设选项：最前面是「自定义」。
fn preset_options() -> Vec<SelectOption> {
    std::iter::once("custom")
        .chain(PRESETS.iter().map(|(id, _, _)| *id))
        .map(|id| SelectOption::new(id, key(&format!("ai.presets.{id}"))))
        .collect()
}

/// 地址对得上哪个预设；对不上是「自定义」。
fn preset_of(base_url: &str) -> &'static str {
    let base_url = base_url.trim().trim_end_matches('/');
    PRESETS
        .iter()
        .find(|(_, url, _)| *url == base_url)
        .map_or("custom", |(id, _, _)| id)
}

/// 翻译设置用到的控件状态，随偏好窗口存活。
pub(super) struct TranslateControls {
    engine: SelectState,
    first: SelectState,
    second: SelectState,
    preset: SelectState,
    base_url: TextInput,
    api_key: TextInput,
    model: TextInput,
    prompt: TextAreaInput,
    /// 已经存盘的 AI 接口配置，内容没变时不重复写。
    saved: TranslateAiConfig,
    /// 还没写盘的最新 AI 接口配置。同一时间只跑一个写入，写完接着写这里最新的一份，
    /// 盘上最后留下的总是最后一次改的内容。
    pending: Rc<Cell<Option<TranslateAiConfig>>>,
    /// 是否有写入在跑。
    writing: Rc<Cell<bool>>,
    /// 「更多选项」（提示词）是否展开，只在这个窗口存活期间保留。
    more: bool,
}

impl TranslateControls {
    pub(super) fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Preferences>,
    ) -> (Self, Vec<Subscription>) {
        let options = &settings.extensions.translate;
        let (first_language, second_language) =
            translate::languages(options, settings.appearance.language);
        let saved = core_host::core(cx)
            .map(|core| core.translate_ai_config())
            .unwrap_or_default();

        let engine = SelectState::new(
            engine_options(),
            Some(engine_value(options.engine)),
            window,
            cx,
        );
        let first = SelectState::new(
            language_options(true, None),
            Some(options.first_language.as_str()),
            window,
            cx,
        );
        let second = SelectState::new(
            language_options(false, Some(first_language.as_str())),
            Some(second_language.as_str()),
            window,
            cx,
        );
        let preset = SelectState::new(
            preset_options(),
            Some(preset_of(&saved.base_url)),
            window,
            cx,
        );
        let base_url = TextInput::new("https://api.example.com/v1", window, cx);
        base_url.set_value(saved.base_url.clone(), window, cx);
        let api_key = TextInput::new("sk-…", window, cx);
        api_key.set_value(saved.api_key.clone(), window, cx);
        let model = TextInput::new(key("ai.model"), window, cx);
        model.set_value(saved.model.clone(), window, cx);
        // 多行框换不了占位文字，标题和说明就在它上面，不另给占位。
        let prompt = TextAreaInput::new(SharedString::default(), 3, 8, window, cx);
        prompt.set_value(saved.prompt.clone(), window, cx);

        let subscriptions = vec![
            engine.on_change(cx, |this, value, cx| {
                if let Some(value) = value {
                    this.update("extensions.translate.engine", json!(value.as_ref()), cx);
                }
            }),
            first.on_change(cx, |this, value, cx| {
                if let Some(value) = value {
                    this.update(
                        "extensions.translate.firstLanguage",
                        json!(value.as_ref()),
                        cx,
                    );
                }
            }),
            second.on_change(cx, |this, value, cx| {
                if let Some(value) = value {
                    this.update(
                        "extensions.translate.secondLanguage",
                        json!(value.as_ref()),
                        cx,
                    );
                }
            }),
            preset.on_change_in(window, cx, |this, value, window, cx| {
                let chosen = value
                    .as_deref()
                    .and_then(|id| PRESETS.iter().find(|(preset, _, _)| *preset == id));
                if let Some((_, url, model)) = chosen {
                    this.translate.base_url.set_value(*url, window, cx);
                    this.translate.model.set_value(*model, window, cx);
                    this.save_translate_ai(cx);
                }
            }),
            base_url.on_commit(window, cx, |this, _, window, cx| {
                this.save_translate_ai(cx);
                let preset = preset_of(&this.translate.saved.base_url);
                this.translate.preset.set_value(preset, window, cx);
            }),
            api_key.on_commit(window, cx, |this, _, _, cx| this.save_translate_ai(cx)),
            model.on_commit(window, cx, |this, _, _, cx| this.save_translate_ai(cx)),
            prompt.on_change(cx, |this, _, cx| this.save_translate_ai(cx)),
        ];
        let controls = Self {
            engine,
            first,
            second,
            preset,
            base_url,
            api_key,
            model,
            prompt,
            saved,
            pending: Rc::default(),
            writing: Rc::default(),
            more: false,
        };
        (controls, subscriptions)
    }

    /// 设置变了：下拉跟着当前设置走，第二语言的选项里去掉首选语言。
    pub(super) fn sync(&self, settings: &Settings, window: &mut Window, cx: &mut App) {
        let options = &settings.extensions.translate;
        let (first, second) = translate::languages(options, settings.appearance.language);
        self.engine.set_options(engine_options(), window, cx);
        self.engine
            .set_value(engine_value(options.engine), window, cx);
        self.first
            .set_options(language_options(true, None), window, cx);
        self.first.set_value(&options.first_language, window, cx);
        self.second
            .set_options(language_options(false, Some(first.as_str())), window, cx);
        self.second.set_value(&second, window, cx);
    }

    /// 切换界面语言后：选项和占位文字换成新语言，跟随界面语言的首选语言也跟着变。
    pub(super) fn relabel(&self, settings: &Settings, window: &mut Window, cx: &mut App) {
        self.preset.set_options(preset_options(), window, cx);
        self.model.set_placeholder(key("ai.model"), window, cx);
        self.sync(settings, window, cx);
    }

    /// 关窗前提交正在编辑的单行输入框：移除窗口不会再发失焦。
    pub(super) fn commit_if_focused(&self, window: &Window, cx: &mut App) {
        for input in [&self.base_url, &self.api_key, &self.model] {
            input.commit_if_focused(window, cx);
        }
    }

    /// 扩展装卸之后重读 AI 接口配置：卸载会把状态文件一起删掉。
    pub(super) fn reload_ai(&mut self, window: &mut Window, cx: &mut App) {
        let ai = core_host::core(cx)
            .map(|core| core.translate_ai_config())
            .unwrap_or_default();
        if ai == self.saved {
            return;
        }
        self.base_url.set_value(ai.base_url.clone(), window, cx);
        self.api_key.set_value(ai.api_key.clone(), window, cx);
        self.model.set_value(ai.model.clone(), window, cx);
        self.prompt.set_value(ai.prompt.clone(), window, cx);
        self.preset.set_value(preset_of(&ai.base_url), window, cx);
        self.saved = ai;
    }
}

impl Preferences {
    /// 把输入框里的内容存成 AI 接口配置；和已存的一样就不写。
    ///
    /// 提示词每敲一个字都会来一次，并行写盘时旧内容可能最后落盘，所以写入排成一个：
    /// 正在写时只记下最新的一份，写完再写它。写入任务不随窗口关闭取消。
    fn save_translate_ai(&mut self, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let controls = &self.translate;
        let ai = TranslateAiConfig {
            base_url: controls.base_url.value(cx).trim().to_owned(),
            api_key: controls.api_key.value(cx).trim().to_owned(),
            model: controls.model.value(cx).trim().to_owned(),
            prompt: controls.prompt.value(cx).to_string(),
        };
        if ai == controls.saved {
            return;
        }
        self.translate.saved = ai.clone();
        self.translate.pending.set(Some(ai));
        if self.translate.writing.replace(true) {
            return;
        }
        let pending = self.translate.pending.clone();
        let writing = self.translate.writing.clone();
        cx.spawn(async move |_, _| {
            while let Some(ai) = pending.take() {
                if let Err(err) = core.set_translate_ai_config(ai).await {
                    log::warn!("translate AI config was not saved: {err}");
                }
            }
            writing.set(false);
        })
        .detach();
    }

    /// 启用中的翻译扩展的设置行，是翻译页的第二块。
    pub(super) fn render_translate(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let controls = &self.translate;
        let options = &self.settings.extensions.translate;
        let select = |state: &SelectState, label: SharedString| {
            Select::new(state)
                .width(CONTROL_WIDTH)
                .accessibility_label(label)
                .into_any_element()
        };
        let input = |state: &TextInput, label: SharedString| {
            Input::new(state)
                .width(CONTROL_WIDTH)
                .accessibility_label(label)
                .into_any_element()
        };
        let none = SharedString::default;

        let mut rows = vec![
            setting_row(
                true,
                key("engine"),
                none(),
                Some(select(&controls.engine, key("engine"))),
                tokens,
            ),
            setting_row(
                false,
                key("firstLanguage"),
                key("firstLanguageDescription"),
                Some(select(&controls.first, key("firstLanguage"))),
                tokens,
            ),
            setting_row(
                false,
                key("secondLanguage"),
                none(),
                Some(select(&controls.second, key("secondLanguage"))),
                tokens,
            ),
            setting_row(
                false,
                key("showOnCards"),
                key("showOnCardsDescription"),
                Some(self.render_lan_switch(
                    "extensions.translate.showOnCards",
                    options.show_on_cards,
                    key("showOnCards"),
                    cx,
                )),
                tokens,
            ),
            setting_row(
                false,
                key("auto"),
                key("autoDescription"),
                Some(self.render_lan_switch(
                    "extensions.translate.auto",
                    options.auto,
                    key("auto"),
                    cx,
                )),
                tokens,
            ),
        ];

        if options.engine == TranslateEngineKind::Ai {
            rows.push(setting_row(
                false,
                key("ai.preset"),
                key("ai.presetDescription"),
                Some(select(&controls.preset, key("ai.preset"))),
                tokens,
            ));
            rows.push(setting_row(
                false,
                key("ai.baseUrl"),
                key("ai.baseUrlDescription"),
                Some(input(&controls.base_url, key("ai.baseUrl"))),
                tokens,
            ));
            rows.push(setting_row(
                false,
                key("ai.apiKey"),
                none(),
                Some(input(&controls.api_key, key("ai.apiKey"))),
                tokens,
            ));
            rows.push(setting_row(
                false,
                key("ai.model"),
                key("ai.modelDescription"),
                Some(input(&controls.model, key("ai.model"))),
                tokens,
            ));
            let expanded = controls.more;
            rows.push(
                row_frame(false, tokens)
                    .child(
                        div()
                            .id("translate-more-options")
                            .flex()
                            .items_center()
                            .gap(space(2.))
                            .flex_1()
                            .cursor_pointer()
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.secondary)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.translate.more = !this.translate.more;
                                cx.notify();
                            }))
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size(rems(1.))
                                    .color(tokens.text.muted)
                                    .when(!expanded, |icon| {
                                        icon.rotate(radians(-std::f32::consts::FRAC_PI_2))
                                    }),
                            )
                            .child(i18n::t("preferences:controls.moreOptions")),
                    )
                    .into_any_element(),
            );
            if expanded {
                rows.push(
                    row_frame(false, tokens)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap(space(2.))
                                .child(row_label(
                                    key("ai.prompt"),
                                    key("ai.promptDescription"),
                                    tokens,
                                ))
                                .child(TextArea::new(&controls.prompt)),
                        )
                        .into_any_element(),
                );
            }
        }

        let clear = {
            let entity = cx.entity().downgrade();
            Button::new("translate-clear", key("clearConfirm")).on_click(move |_, window, cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, cx| this.clear_translations(window, cx));
                }
            })
        };
        rows.push(setting_row(
            false,
            key("clear"),
            key("clearDescription"),
            Some(clear.into_any_element()),
            tokens,
        ));
        rows
    }

    /// 确认后删掉所有译文，结果用提示告知。
    fn clear_translations(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let answer = form_dialog(
            DialogSpec::new(key("clear"))
                .ok_text(key("clearConfirm"))
                .cancel_text(i18n::t("common:actions.cancel"))
                .danger(),
            move |_, _| {
                div()
                    .kp_text(TextSize::Sm)
                    .child(key("clearContent"))
                    .into_any_element()
            },
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let result = core.clear_translations().await;
                let _ = entity.update_in(cx, |_, window, cx| {
                    let toast = match result {
                        Ok(()) => Toast::success(key("cleared")),
                        Err(error) => Toast::error(error.to_string()),
                    };
                    toast::show(toast, window, cx);
                });
            })
            .detach();
    }
}
