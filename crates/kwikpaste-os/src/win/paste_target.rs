//! Pure HWND identity and short-lived paste handoff policy, independent of Win32.

use crate::paste_target::PasteTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowTarget {
    pub window: isize,
    pub process_id: u32,
}

/// A retained HWND is useful only while its captured external owner still owns it.
pub fn is_valid_target(target: WindowTarget, owner: Option<u32>, own_process: u32) -> bool {
    target.window != 0
        && target.process_id != 0
        && target.process_id != own_process
        && owner == Some(target.process_id)
}

#[derive(Default)]
pub struct PasteSession {
    pub origin: Option<WindowTarget>,
    generation: u64,
    pending: Option<PasteTarget>,
}

impl PasteSession {
    /// Showing starts a fresh handoff generation, retaining an origin only within a visible session.
    pub fn show(&mut self, current: Option<WindowTarget>, visible: bool) {
        self.cancel();
        if !visible || current.is_some() {
            self.origin = current;
        }
    }

    /// Prefer the current valid external destination over the retained origin.
    pub fn begin(
        &mut self,
        current: Option<WindowTarget>,
        valid: impl Fn(WindowTarget) -> bool,
    ) -> Option<PasteTarget> {
        self.cancel();
        current
            .filter(|target| valid(*target))
            .or_else(|| self.origin.filter(|target| valid(*target)))
            .map(|target| {
                let ticket = PasteTarget {
                    generation: self.generation,
                    window: target.window,
                    process_id: target.process_id,
                };
                self.pending = Some(ticket);
                ticket
            })
    }

    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }

    pub fn is_current(&self, target: PasteTarget) -> bool {
        self.pending == Some(target) && self.generation == target.generation
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandoffError {
    Superseded,
    InvalidOwner,
    ForegroundChanged,
}

/// Await native input release and exact foreground identity; a different external window cancels.
pub fn handoff_ready(
    session: &PasteSession,
    target: PasteTarget,
    owner: Option<u32>,
    own_process: u32,
    foreground: isize,
    panel: isize,
    input_released: bool,
) -> Result<bool, HandoffError> {
    if !session.is_current(target) {
        return Err(HandoffError::Superseded);
    }
    let destination = WindowTarget {
        window: target.window,
        process_id: target.process_id,
    };
    if !is_valid_target(destination, owner, own_process) {
        return Err(HandoffError::InvalidOwner);
    }
    if foreground != 0 && foreground != panel && foreground != target.window {
        return Err(HandoffError::ForegroundChanged);
    }

    Ok(input_released && foreground == target.window)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 10;
    const PANEL: isize = 500;
    const A: WindowTarget = WindowTarget {
        window: 100,
        process_id: 20,
    };
    const B: WindowTarget = WindowTarget {
        window: 200,
        process_id: 30,
    };

    fn begun() -> (PasteSession, PasteTarget) {
        let mut session = PasteSession::default();
        session.show(Some(A), false);
        let ticket = session.begin(Some(A), |_| true).expect("valid target");
        (session, ticket)
    }

    #[test]
    fn rejects_dead_self_zero_and_other_process_reused_handles() {
        assert!(!is_valid_target(A, None, OWN));
        assert!(!is_valid_target(A, Some(B.process_id), OWN));
        assert!(!is_valid_target(
            WindowTarget {
                process_id: OWN,
                ..A
            },
            Some(OWN),
            OWN
        ));
        assert!(!is_valid_target(
            WindowTarget { window: 0, ..A },
            Some(A.process_id),
            OWN
        ));
        assert!(!is_valid_target(
            WindowTarget { process_id: 0, ..A },
            Some(0),
            OWN
        ));
        assert!(is_valid_target(A, Some(A.process_id), OWN));
    }

    #[test]
    fn current_valid_external_destination_wins_over_origin() {
        let mut session = PasteSession::default();
        session.show(Some(A), false);
        let ticket = session
            .begin(Some(B), |_| true)
            .expect("current destination");
        assert_eq!((ticket.window, ticket.process_id), (B.window, B.process_id));
    }

    #[test]
    fn invalid_current_falls_back_only_to_a_valid_retained_origin() {
        let mut session = PasteSession::default();
        session.show(Some(A), false);
        let ticket = session
            .begin(Some(B), |target| target == A)
            .expect("retained target");
        assert_eq!(ticket.window, A.window);
        assert!(session.begin(None, |_| false).is_none());
        assert!(!session.is_current(ticket));
    }

    #[test]
    fn visible_editing_preserves_origin_but_a_new_hidden_session_clears_it() {
        let mut session = PasteSession::default();
        session.show(Some(A), false);
        session.show(None, true);
        assert_eq!(session.origin, Some(A));
        session.show(None, false);
        assert_eq!(session.origin, None);
    }

    #[test]
    fn repeated_request_and_explicit_cancel_invalidate_old_tickets() {
        let (mut session, first) = begun();
        let second = session.begin(Some(A), |_| true).expect("second target");
        assert!(!session.is_current(first));
        assert!(session.is_current(second));
        session.cancel();
        assert!(!session.is_current(second));
    }

    #[test]
    fn a_new_show_invalidates_a_pending_ticket() {
        let (mut session, ticket) = begun();
        session.show(Some(B), true);
        assert_eq!(
            handoff_ready(
                &session,
                ticket,
                Some(A.process_id),
                OWN,
                A.window,
                PANEL,
                true
            ),
            Err(HandoffError::Superseded)
        );
    }

    #[test]
    fn generation_alone_does_not_authorize_a_different_destination() {
        let (session, ticket) = begun();
        assert!(!session.is_current(PasteTarget {
            window: B.window,
            process_id: B.process_id,
            ..ticket
        }));
    }

    #[test]
    fn owner_mismatch_is_terminal_even_if_the_hwnd_is_foreground() {
        let (session, ticket) = begun();
        assert_eq!(
            handoff_ready(
                &session,
                ticket,
                Some(B.process_id),
                OWN,
                A.window,
                PANEL,
                true
            ),
            Err(HandoffError::InvalidOwner)
        );
    }

    #[test]
    fn user_changed_foreground_is_terminal_instead_of_being_reclaimed() {
        let (session, ticket) = begun();
        assert_eq!(
            handoff_ready(
                &session,
                ticket,
                Some(A.process_id),
                OWN,
                B.window,
                PANEL,
                true
            ),
            Err(HandoffError::ForegroundChanged)
        );
    }

    #[test]
    fn readiness_waits_for_native_release_and_actual_target_foreground() {
        let (session, ticket) = begun();
        assert_eq!(
            handoff_ready(
                &session,
                ticket,
                Some(A.process_id),
                OWN,
                A.window,
                PANEL,
                false
            ),
            Ok(false)
        );
        assert_eq!(
            handoff_ready(
                &session,
                ticket,
                Some(A.process_id),
                OWN,
                PANEL,
                PANEL,
                true
            ),
            Ok(false)
        );
        assert_eq!(
            handoff_ready(&session, ticket, Some(A.process_id), OWN, 0, PANEL, true),
            Ok(false)
        );
        assert_eq!(
            handoff_ready(
                &session,
                ticket,
                Some(A.process_id),
                OWN,
                A.window,
                PANEL,
                true
            ),
            Ok(true)
        );
    }
}
