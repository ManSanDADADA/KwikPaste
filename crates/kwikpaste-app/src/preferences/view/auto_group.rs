//! 按来源应用自动归组：列表沿用文本匹配规则的样式，编辑弹框是分组下拉加应用勾选列表。
use super::*;
use crate::clipboard::source::Group;
use kwikpaste_core::{ops::ClipboardAppView, settings::AutoGroupRule};

const RULES_PATH: &str = "clipboard.autoGroupRules";

fn words(key: &str) -> gpui::SharedString {
    i18n::t(&format!("preferences:autoGroup.{key}"))
}

/// 规则行里的应用名：优先取已知应用的名称，取不到时用 exe 文件名或 bundle id。
fn app_label(id: &str, apps: &[ClipboardAppView]) -> String {
    if let Some(app) = apps
        .iter()
        .find(|app| !app.name.is_empty() && app_ids::same_app(&app.id, id))
    {
        return app.name.clone();
    }
    let tail = id.rsplit(['/', '\\']).next().unwrap_or(id);
    tail.strip_suffix(".exe").unwrap_or(tail).to_owned()
}

impl Preferences {
    fn update_auto_group_rules(&mut self, rules: Vec<AutoGroupRule>, cx: &mut Context<Self>) {
        self.update(RULES_PATH, json!(rules), cx);
    }

    pub(super) fn refresh_auto_group_apps(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.clipboard.auto_group_rules.is_empty() {
            return;
        }
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            match core.list_all_apps().await {
                Ok(apps) => {
                    let _ = this.update(cx, |this, cx| {
                        this.auto_group_apps = apps;
                        cx.notify();
                    });
                }
                Err(error) => log::warn!("auto group apps could not be loaded: {error:#}"),
            }
        })
        .detach();
    }

    /// 先取可选应用（含运行中的）再打开编辑弹框；`existing` 为 `None` 时新增规则。
    fn open_auto_group_rule(
        &self,
        existing: Option<AutoGroupRule>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let editing = existing.is_some();
        // 新规则的 id 到保存时才分配（见 `save_rule`）：取应用列表期间可能又开了一个弹框。
        let base = existing.unwrap_or_default();
        let groups = self
            .groups
            .iter()
            .map(|group| (group.id.clone(), group.name.clone()))
            .collect();
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let apps = match core.list_all_apps().await {
                    Ok(apps) => apps,
                    Err(error) => {
                        log::warn!("could not load source apps: {error:#}");
                        return;
                    }
                };
                let _ = cx.update(|window, cx| {
                    open_rule_editor(base, editing, groups, apps, core, entity, window, cx);
                });
            })
            .detach();
    }

    pub(super) fn render_auto_group_rules(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let rules = &self.settings.clipboard.auto_group_rules;
        let rows = rules.iter().enumerate().map(|(index, rule)| {
            let toggle = entity.clone();
            let edit = entity.clone();
            let remove = entity.clone();
            let id = rule.id.clone();
            let delete_id = id.clone();
            let base = rule.clone();
            let group = self
                .groups
                .iter()
                .find(|group| group.id == rule.group_id)
                .map_or_else(|| words("missingGroup"), |group| group.name.clone().into());
            let apps = rule
                .app_ids
                .iter()
                .map(|id| app_label(id, &self.auto_group_apps))
                .collect::<Vec<_>>()
                .join("、");
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
                    Switch::new(format!("auto-group-enabled-{index}"))
                        .small()
                        .checked(rule.enabled)
                        .accessibility_label(group.clone())
                        .on_change(move |enabled, _, cx| {
                            if let Some(entity) = toggle.upgrade() {
                                entity.update(cx, |this, cx| {
                                    let mut rules =
                                        this.settings.clipboard.auto_group_rules.clone();
                                    if let Some(rule) = rules.iter_mut().find(|rule| rule.id == id)
                                    {
                                        rule.enabled = enabled;
                                    }
                                    this.update_auto_group_rules(rules, cx);
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
                        .child(div().kp_text(TextSize::Sm).truncate().child(group))
                        .child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.secondary)
                                .truncate()
                                .child(apps),
                        ),
                )
                .child(
                    Button::new(format!("auto-group-edit-{index}"), words("edit"))
                        .small()
                        .ghost()
                        .on_click(move |_, window, cx| {
                            if let Some(entity) = edit.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.open_auto_group_rule(Some(base.clone()), window, cx)
                                });
                            }
                        }),
                )
                .child(
                    Button::new(format!("auto-group-delete-{index}"), words("delete"))
                        .small()
                        .danger_outline()
                        .on_click(move |_, _, cx| {
                            if let Some(entity) = remove.upgrade() {
                                entity.update(cx, |this, cx| {
                                    let rules = this
                                        .settings
                                        .clipboard
                                        .auto_group_rules
                                        .iter()
                                        .filter(|rule| rule.id != delete_id)
                                        .cloned()
                                        .collect();
                                    this.update_auto_group_rules(rules, cx);
                                });
                            }
                        }),
                )
        });
        let add = entity.clone();
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
                    Button::new("auto-group-add", words("add"))
                        .small()
                        .with_icon(IconName::Plus)
                        .on_click(move |_, window, cx| {
                            if let Some(entity) = add.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.open_auto_group_rule(None, window, cx)
                                });
                            }
                        }),
                ),
            )
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn open_rule_editor(
    base: AutoGroupRule,
    editing: bool,
    groups: Vec<(String, String)>,
    apps: Vec<ClipboardAppView>,
    core: kwikpaste_core::Core,
    parent: WeakEntity<Preferences>,
    window: &mut Window,
    cx: &mut App,
) {
    let icons = cx.new(|_| KpImageCache::with_capacity(128));
    let list = cx.new(|cx| {
        cx.observe(&icons, |_, _, cx| cx.notify()).detach();
        SourceAppsDialog {
            apps,
            selected: base.app_ids.clone(),
            icons,
        }
    });
    let source: Arc<dyn ClipboardSource> = Arc::new(
        clipboard::source::core_source::CoreSource::new(core.clone()),
    );
    let editor = cx.new(|cx| RuleEditor::new(base, groups, list.clone(), source, window, cx));
    let content = editor.clone();
    let check = editor.clone();
    let adder = list.downgrade();
    let answer = form_dialog(
        DialogSpec::new(words(if editing { "edit" } else { "add" }))
            .ok_text(i18n::t("common:actions.save"))
            .cancel_text(i18n::t("common:actions.cancel"))
            .validate(move |_, cx| {
                check.update(cx, |editor, cx| {
                    editor.submitted = true;
                    cx.notify();
                    editor.is_valid(cx)
                })
            })
            .footer_extra(move |_, _| add_app_button(adder.clone(), core.clone())),
        move |_, _| content.clone().into_any_element(),
        window,
        cx,
    );
    window
        .spawn(cx, async move |cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let Ok((rule, apps)) =
                cx.update(|_, cx| (editor.read(cx).to_rule(cx), list.read(cx).apps.clone()))
            else {
                return;
            };
            let _ = parent.update(cx, |this, cx| {
                this.auto_group_apps = apps;
                let mut rules = this.settings.clipboard.auto_group_rules.clone();
                if save_rule(&mut rules, rule, editing) {
                    this.update_auto_group_rules(rules, cx);
                }
            });
        })
        .detach();
}

/// 把弹框里保存的规则并进当前规则：新规则这时才取一个没用过的 id；
/// 编辑期间原规则已被删掉就丢弃这次保存，不让它复活。返回是否有改动。
fn save_rule(rules: &mut Vec<AutoGroupRule>, mut rule: AutoGroupRule, editing: bool) -> bool {
    if editing {
        let Some(current) = rules.iter_mut().find(|current| current.id == rule.id) else {
            return false;
        };
        *current = rule;
        return true;
    }
    rule.id = (1..=rules.len() + 1)
        .map(|i| format!("auto-group-{i}"))
        .find(|id| !rules.iter().any(|rule| rule.id == *id))
        .unwrap_or_else(|| format!("auto-group-{}", rules.len() + 1));
    rules.push(rule);
    true
}

struct RuleEditor {
    base: AutoGroupRule,
    /// 分组下拉的选项（id、名称）；弹框里新建的分组追加在末尾。
    groups: Vec<(String, String)>,
    group: SelectState,
    apps: gpui::Entity<SourceAppsDialog>,
    source: Arc<dyn ClipboardSource>,
    /// 点过保存后才提示未填项，刚打开时不报错。
    submitted: bool,
}

impl RuleEditor {
    fn new(
        base: AutoGroupRule,
        groups: Vec<(String, String)>,
        apps: gpui::Entity<SourceAppsDialog>,
        source: Arc<dyn ClipboardSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let group = SelectState::new(
            group_options(&groups),
            Some(base.group_id.as_str()),
            window,
            cx,
        );
        cx.observe(&apps, |_, _, cx| cx.notify()).detach();
        Self {
            base,
            groups,
            group,
            apps,
            source,
            submitted: false,
        }
    }

    fn is_valid(&self, cx: &App) -> bool {
        self.group
            .selected_value(cx)
            .is_some_and(|value| !value.is_empty())
            && !self.apps.read(cx).selected.is_empty()
    }

    fn to_rule(&self, cx: &App) -> AutoGroupRule {
        let mut rule = self.base.clone();
        rule.group_id = self
            .group
            .selected_value(cx)
            .map(|value| value.to_string())
            .unwrap_or_default();
        rule.app_ids = self.apps.read(cx).selected.clone();
        rule
    }

    fn add_group(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        self.groups
            .push((group.id.to_string(), group.name.to_string()));
        self.group
            .set_options(group_options(&self.groups), window, cx);
        self.group.set_value(&group.id, window, cx);
        cx.notify();
    }
}

fn group_options(groups: &[(String, String)]) -> Vec<SelectOption> {
    groups
        .iter()
        .map(|(id, name)| SelectOption::new(id.clone(), name.clone()))
        .collect()
}

impl Render for RuleEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let source = self.source.clone();
        let error = (self.submitted && !self.is_valid(cx)).then(|| words("required"));
        div()
            .flex()
            .flex_col()
            .gap(space(3.))
            .child(form_field(
                words("group"),
                div()
                    .flex()
                    .items_center()
                    .gap(space(2.))
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&self.group)
                                .placeholder(words("pickGroup"))
                                .accessibility_label(words("group")),
                        ),
                    )
                    .child(
                        Button::new("auto-group-new-group", words("newGroup"))
                            .with_icon(IconName::Plus)
                            .on_click(move |_, window, cx| {
                                let entity = entity.clone();
                                group_dialogs::edit_group(
                                    source.clone(),
                                    None,
                                    move |group, window, cx| {
                                        let _ = entity.update(cx, |editor, cx| {
                                            editor.add_group(group, window, cx)
                                        });
                                    },
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                words("apps"),
                self.apps.clone().into_any_element(),
                tokens,
            ))
            .when_some(error, |view, error| {
                view.child(
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.status.danger.solid)
                        .child(error),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rules_get_unique_ids_on_save_and_edits_of_deleted_rules_are_dropped() {
        let mut rules = Vec::new();
        // 两个同时打开的新增弹框：各自保存时都拿到没用过的 id。
        assert!(save_rule(&mut rules, AutoGroupRule::default(), false));
        assert!(save_rule(&mut rules, AutoGroupRule::default(), false));
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            ["auto-group-1", "auto-group-2"]
        );

        let mut edited = rules[0].clone();
        edited.group_id = "work".to_owned();
        assert!(save_rule(&mut rules, edited, true));
        assert_eq!(rules[0].group_id, "work");

        let deleted = rules.remove(1);
        assert!(!save_rule(&mut rules, deleted, true));
        assert_eq!(rules.len(), 1);
    }
}
