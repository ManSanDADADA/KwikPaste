//! 不触碰 AppKit 的粘贴会话与焦点判定；保留的应用对象由面板持有。

pub(super) struct HandoffSession<T> {
    generation: u64,
    target: Option<T>,
}

impl<T> Default for HandoffSession<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            target: None,
        }
    }
}

impl<T> HandoffSession<T> {
    pub(super) fn begin(&mut self, target: T) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.target = Some(target);
        self.generation
    }

    pub(super) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.target = None;
    }

    pub(super) fn target(&self, generation: u64) -> Option<&T> {
        self.target
            .as_ref()
            .filter(|_| generation == self.generation)
    }

    pub(super) fn pending(&self) -> Option<&T> {
        self.target.as_ref()
    }
}

pub(super) fn select_target<T>(
    current: Option<T>,
    retained: Option<T>,
    valid: impl Fn(&T) -> bool,
) -> Option<T> {
    current.filter(&valid).or_else(|| retained.filter(valid))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Foreground {
    Target,
    OwnOrMissing,
    OtherExternal,
}

/// 只有目标活着、确为前台且面板已交还键盘时才允许注入。
pub(super) fn ready(
    target_live: bool,
    target_active: bool,
    foreground: Foreground,
    panel_key: bool,
) -> Result<bool, &'static str> {
    if !target_live {
        return Err("paste target is no longer running");
    }
    if foreground == Foreground::OtherExternal {
        return Err("paste target changed while handing off keyboard focus");
    }
    Ok(target_active && foreground == Foreground::Target && !panel_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_show_or_cancel_rejects_the_old_handoff() {
        let mut session = HandoffSession::default();
        let ticket = session.begin("first");
        session.cancel();
        assert_eq!(session.target(ticket), None);
    }

    #[test]
    fn second_paste_rejects_the_first_ticket() {
        let mut session = HandoffSession::default();
        let first = session.begin("first");
        let second = session.begin("second");
        assert_eq!(session.target(first), None);
        assert_eq!(session.target(second), Some(&"second"));
    }

    #[test]
    fn invalid_begin_cannot_leave_an_old_handoff_usable() {
        let mut session = HandoffSession::default();
        let first = session.begin("old");
        session.cancel();
        assert_eq!(select_target(None::<&str>, None, |_| true), None);
        assert_eq!(session.target(first), None);
    }

    #[test]
    fn current_external_target_wins_over_retained_target() {
        assert_eq!(
            select_target(Some("new"), Some("old"), |_| true),
            Some("new")
        );
    }

    #[test]
    fn own_and_dead_targets_are_never_selected() {
        let valid = |target: &&str| *target != "own" && *target != "dead";
        assert_eq!(select_target(Some("own"), Some("dead"), valid), None);
        assert_eq!(
            select_target(Some("dead"), Some("live"), valid),
            Some("live")
        );
    }

    #[test]
    fn pinned_visible_panel_must_release_keyboard_before_paste() {
        assert_eq!(ready(true, true, Foreground::Target, true), Ok(false));
        assert_eq!(ready(true, true, Foreground::Target, false), Ok(true));
    }

    #[test]
    fn switching_to_another_external_app_cancels_instead_of_reactivating_origin() {
        assert!(ready(true, false, Foreground::OtherExternal, false).is_err());
        assert_eq!(
            ready(true, false, Foreground::OwnOrMissing, false),
            Ok(false)
        );
    }

    #[test]
    fn terminated_or_not_yet_active_target_cannot_receive_paste() {
        assert!(ready(false, true, Foreground::Target, false).is_err());
        assert_eq!(ready(true, false, Foreground::Target, false), Ok(false));
    }
}
