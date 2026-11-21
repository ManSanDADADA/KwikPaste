//! 自定义文本规则沿用清理规则的列表与编辑弹框；测试文本只保留在当前弹框。
use super::*;
use kwikpaste_core::clipboard::rules::{
    CustomRule, MAX_CUSTOM_RULES, RuleAction, RuleError, TEMPLATES, compile_pattern,
};
use kwikpaste_ui::{TextArea, TextAreaInput};

fn words(key: &str) -> gpui::SharedString {
    i18n::t(&format!("preferences:customRules.{key}"))
}
fn action_key(action: RuleAction) -> &'static str {
    match action {
        RuleAction::Snippet => "snippet",
        RuleAction::Sensitive => "sensitive",
        RuleAction::Ignore => "ignore",
    }
}
fn error_text(error: RuleError) -> &'static str {
    error.message(match i18n::language() {
        i18n::Language::ZhCn => kwikpaste_core::settings::Language::ZhCN,
        i18n::Language::EnUs => kwikpaste_core::settings::Language::EnUS,
    })
}

impl Preferences {
    fn update_custom_rules(&mut self, rules: Vec<CustomRule>, cx: &mut Context<Self>) {
        self.update(
            kwikpaste_core::settings::paths::CUSTOM_RULES,
            json!(rules),
            cx,
        );
    }

    /// 新增、模板和编辑共用表单，只有通过校验后才写设置。
    fn open_custom_rule(
        &self,
        existing: Option<CustomRule>,
        template: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rules = &self.settings.clipboard.custom_rules;
        let base = existing.unwrap_or_else(|| {
            let id = (1..=rules.len() + 1)
                .map(|i| format!("text-rule-{i}"))
                .find(|id| !rules.iter().any(|rule| rule.id == *id))
                .unwrap_or_else(|| format!("text-rule-{}", rules.len() + 1));
            let (name, action, pattern) = template
                .and_then(|index| TEMPLATES.get(index))
                .copied()
                .unwrap_or(("new", RuleAction::Snippet, ""));
            CustomRule {
                id,
                name: words(&format!("templates.{name}")).to_string(),
                pattern: pattern.to_owned(),
                action,
                enabled: true,
            }
        });
        let editor = cx.new(|cx| RuleEditor::new(base, window, cx));
        let content = editor.clone();
        let check = editor.clone();
        let answer = form_dialog(
            DialogSpec::new(words("edit"))
                .ok_text(i18n::t("common:actions.save"))
                .cancel_text(i18n::t("common:actions.cancel"))
                .validate(move |_, cx| check.read(cx).validation(cx).is_ok()),
            move |_, _| content.clone().into_any_element(),
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let Ok(rule) = cx.update(|_, cx| editor.read(cx).to_rule(cx)) else {
                    return;
                };
                let _ = entity.update(cx, |this, cx| {
                    let mut rules = this.settings.clipboard.custom_rules.clone();
                    if let Some(current) = rules.iter_mut().find(|current| current.id == rule.id) {
                        *current = rule;
                    } else {
                        rules.push(rule);
                    }
                    this.update_custom_rules(rules, cx);
                });
            })
            .detach();
    }

    pub(super) fn render_custom_rules(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let rules = &self.settings.clipboard.custom_rules;
        let rows = rules.iter().enumerate().map(|(index, rule)| {
            let toggle = entity.clone();
            let edit = entity.clone();
            let remove = entity.clone();
            let id = rule.id.clone();
            let delete_id = id.clone();
            let base = rule.clone();
            div()
                .flex()
                .items_center()
                .gap(space(2.))
                .px(space(3.))
                .py(space(2.))
                .when(index > 0, |row| {
                    row.border_t_1().border_color(tokens.border.divider)
                })
                .child(
                    Switch::new(format!("custom-enabled-{index}"))
                        .small()
                        .checked(rule.enabled)
                        .accessibility_label(rule.name.clone())
                        .on_change(move |enabled, _, cx| {
                            if let Some(entity) = toggle.upgrade() {
                                entity.update(cx, |this, cx| {
                                    let mut rules = this.settings.clipboard.custom_rules.clone();
                                    if let Some(rule) = rules.iter_mut().find(|rule| rule.id == id)
                                    {
                                        rule.enabled = enabled;
                                    }
                                    this.update_custom_rules(rules, cx);
                                });
                            }
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .kp_text(TextSize::Sm)
                                .truncate()
                                .child(rule.name.clone()),
                        )
                        .child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.secondary)
                                .child(words(&format!("actions.{}", action_key(rule.action)))),
                        ),
                )
                .child(
                    Button::new(format!("custom-edit-{index}"), words("edit"))
                        .small()
                        .ghost()
                        .on_click(move |_, window, cx| {
                            if let Some(entity) = edit.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.open_custom_rule(Some(base.clone()), None, window, cx)
                                });
                            }
                        }),
                )
                .child(
                    Button::new(format!("custom-delete-{index}"), words("delete"))
                        .small()
                        .danger_outline()
                        .on_click(move |_, _, cx| {
                            if let Some(entity) = remove.upgrade() {
                                entity.update(cx, |this, cx| {
                                    let rules = this
                                        .settings
                                        .clipboard
                                        .custom_rules
                                        .iter()
                                        .filter(|rule| rule.id != delete_id)
                                        .cloned()
                                        .collect();
                                    this.update_custom_rules(rules, cx);
                                });
                            }
                        }),
                )
        });
        let add = entity.clone();
        let template_buttons = TEMPLATES.iter().enumerate().map(|(index, (key, _, _))| {
            let entity = entity.clone();
            Button::new(
                format!("custom-template-{index}"),
                words(&format!("templates.{key}")),
            )
            .small()
            .ghost()
            .disabled(rules.len() >= MAX_CUSTOM_RULES)
            .on_click(move |_, window, cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, cx| {
                        this.open_custom_rule(None, Some(index), window, cx)
                    });
                }
            })
        });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(space(2.))
            .child(if rules.is_empty() {
                list_tile(tokens)
                    .p(space(3.))
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.secondary)
                    .child(words("empty"))
                    .into_any_element()
            } else {
                list_tile(tokens).children(rows).into_any_element()
            })
            .child(
                div().flex().child(
                    Button::new("custom-add", words("add"))
                        .small()
                        .with_icon(IconName::Plus)
                        .disabled(rules.len() >= MAX_CUSTOM_RULES)
                        .on_click(move |_, window, cx| {
                            if let Some(entity) = add.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.open_custom_rule(None, None, window, cx)
                                });
                            }
                        }),
                ),
            )
            .child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.secondary)
                    .child(words("fromTemplate")),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(1.))
                    .children(template_buttons),
            )
            .into_any_element()
    }
}

struct RuleEditor {
    base: CustomRule,
    name: TextInput,
    pattern: TextInput,
    action: SelectState,
    test: TextAreaInput,
    pattern_error: Option<RuleError>,
    // 存储测试结果而非正文，避免渲染时重新编译或扫描。
    matches: Vec<String>,
    match_count: usize,
    _subscriptions: Vec<Subscription>,
}
impl RuleEditor {
    fn new(base: CustomRule, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = TextInput::new(words("name"), window, cx);
        name.set_value(base.name.clone(), window, cx);
        let pattern = TextInput::new(words("pattern"), window, cx);
        pattern.set_value(base.pattern.clone(), window, cx);
        let action = SelectState::new(
            [
                RuleAction::Snippet,
                RuleAction::Sensitive,
                RuleAction::Ignore,
            ]
            .into_iter()
            .map(|action| {
                SelectOption::new(
                    action_key(action),
                    words(&format!("actions.{}", action_key(action))),
                )
            })
            .collect(),
            Some(action_key(base.action)),
            window,
            cx,
        );
        let test = TextAreaInput::new(words("test"), 3, 5, window, cx);
        let subscriptions = vec![
            name.on_change(cx, |_, _, cx| cx.notify()),
            pattern.on_change(cx, |this, _, cx| this.refresh_matches(cx)),
            test.on_change(cx, |this, _, cx| this.refresh_matches(cx)),
        ];
        let pattern_error = compile_pattern(&base.pattern).err();
        Self {
            base,
            name,
            pattern,
            action,
            test,
            pattern_error,
            matches: Vec::new(),
            match_count: 0,
            _subscriptions: subscriptions,
        }
    }

    fn to_rule(&self, cx: &App) -> CustomRule {
        let mut rule = self.base.clone();
        rule.name = self.name.value(cx).trim().to_owned();
        rule.pattern = self.pattern.value(cx).to_string();
        rule.action = match self.action.selected_value(cx).as_deref() {
            Some("sensitive") => RuleAction::Sensitive,
            Some("ignore") => RuleAction::Ignore,
            _ => RuleAction::Snippet,
        };
        rule
    }
    fn validation(&self, cx: &App) -> Result<(), RuleError> {
        if self.name.value(cx).trim().is_empty() {
            return Err(RuleError::Name);
        }
        self.pattern_error.map_or(Ok(()), Err)
    }
    /// 输入变化时编译一次并测试；结果最多展示 32 个，但计数保持准确。
    fn refresh_matches(&mut self, cx: &mut Context<Self>) {
        self.matches.clear();
        self.match_count = 0;
        match compile_pattern(&self.pattern.value(cx)) {
            Ok(regex) => {
                self.pattern_error = None;
                let test = self.test.value(cx);
                for range in regex.find_iter(&test) {
                    self.match_count += 1;
                    if self.matches.len() < 32 {
                        self.matches.push(test[range].chars().take(200).collect());
                    }
                }
            }
            Err(error) => self.pattern_error = Some(error),
        }
        cx.notify();
    }
}
impl Render for RuleEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        div()
            .flex()
            .flex_col()
            .gap(space(3.))
            .child(form_field(
                words("name"),
                Input::new(&self.name).into_any_element(),
                tokens,
            ))
            .child(form_field(
                words("pattern"),
                Input::new(&self.pattern).into_any_element(),
                tokens,
            ))
            .when_some(self.validation(cx).err(), |view, error| {
                view.child(
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.status.danger.solid)
                        .child(error_text(error)),
                )
            })
            .child(form_field(
                words("action"),
                Select::new(&self.action).into_any_element(),
                tokens,
            ))
            .child(form_field(
                words("test"),
                TextArea::new(&self.test).into_any_element(),
                tokens,
            ))
            .child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.secondary)
                    .child(i18n::t_args(
                        "preferences:customRules.matches",
                        &[("count", &self.match_count.to_string())],
                    )),
            )
            .child(
                div()
                    .max_h(rems(6.))
                    .overflow_hidden()
                    .kp_text(TextSize::Sm)
                    .child(self.matches.join(" · ")),
            )
    }
}
