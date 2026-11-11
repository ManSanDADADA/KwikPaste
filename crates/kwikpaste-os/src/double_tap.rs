//! 只观察物理修饰键的双击状态机；时间戳由平台或测试注入，单位毫秒。

use kwikpaste_core::settings::{DoubleTap, DoubleTapKey, KeySide};

const MAX_HOLD_MS: u64 = 300;
const MAX_GAP_MS: u64 = 300;

/// 一枚有明确左右位置的物理修饰键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalKey {
    pub key: DoubleTapKey,
    pub side: KeySide,
}

impl PhysicalKey {
    fn bit(self) -> u8 {
        let offset = match self.key {
            DoubleTapKey::Control => 0,
            DoubleTapKey::Shift => 2,
            DoubleTapKey::Alt => 4,
            DoubleTapKey::Command => 6,
            DoubleTapKey::Disabled => return 0,
        };
        match self.side {
            KeySide::Left => 1 << offset,
            KeySide::Right => 1 << (offset + 1),
            KeySide::Any => 0,
        }
    }
}

/// 保存第一击松开与当前按住状态；不读取系统、不吞键、不分配内存。
pub struct Detector {
    config: DoubleTap,
    pressed: u8,
    first: Option<(PhysicalKey, u64)>,
    held: Option<(PhysicalKey, u64, bool)>,
}

impl Detector {
    pub const fn new(config: DoubleTap) -> Self {
        Self {
            config,
            pressed: 0,
            first: None,
            held: None,
        }
    }

    /// 配置改变时丢弃未完成手势，但保留按住状态，防止重复按下被当成新一击。
    pub fn configure(&mut self, config: DoubleTap) {
        if self.config != config {
            self.config = config;
            self.reset();
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.key != DoubleTapKey::Disabled
    }

    /// 其它键按下、暂停或输入上下文改变后，不允许旧手势继续。
    pub fn reset(&mut self) {
        self.first = None;
        self.held = None;
    }

    /// 清掉 `is_down` 判定已松开的按住位：松开事件没送到钩子（如 Ctrl+Alt+Del）时不让脏位留下。
    pub fn clear_released(&mut self, is_down: impl Fn(PhysicalKey) -> bool) {
        for key in [
            DoubleTapKey::Control,
            DoubleTapKey::Shift,
            DoubleTapKey::Alt,
        ] {
            for side in [KeySide::Left, KeySide::Right] {
                let physical = PhysicalKey { key, side };
                if self.pressed & physical.bit() != 0 && !is_down(physical) {
                    self.pressed &= !physical.bit();
                }
            }
        }
    }

    /// 输入物理修饰键的按下/松开；只在合格的第二次松开返回 true。
    pub fn modifier(&mut self, physical: PhysicalKey, down: bool, timestamp: u64) -> bool {
        let bit = physical.bit();
        if bit == 0 {
            self.reset();
            return false;
        }
        if down {
            if self.pressed & bit != 0 {
                return false;
            }
            let overlapping = self.pressed != 0;
            self.pressed |= bit;
            if overlapping
                || physical.key != self.config.key
                || (self.config.side != KeySide::Any && self.config.side != physical.side)
            {
                self.reset();
                return false;
            }
            let second = self.first.take().is_some_and(|(key, up)| {
                key == physical
                    && timestamp
                        .checked_sub(up)
                        .is_some_and(|gap| gap <= MAX_GAP_MS)
            });
            self.held = Some((physical, timestamp, second));
            return false;
        }

        self.pressed &= !bit;
        let Some((key, start, second)) = self.held else {
            return false;
        };
        if key != physical {
            return false;
        }
        self.held = None;
        if timestamp
            .checked_sub(start)
            .is_none_or(|held| held > MAX_HOLD_MS)
        {
            self.reset();
            return false;
        }
        if second {
            self.reset();
            return true;
        }
        self.first = Some((physical, timestamp));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: DoubleTapKey, side: KeySide) -> PhysicalKey {
        PhysicalKey { key, side }
    }

    fn detector(key: DoubleTapKey, side: KeySide) -> Detector {
        Detector::new(DoubleTap { key, side })
    }

    fn tap(detector: &mut Detector, key: PhysicalKey, down: u64, up: u64) -> bool {
        assert!(!detector.modifier(key, true, down));
        detector.modifier(key, false, up)
    }

    #[test]
    fn every_key_and_side_fires_only_on_second_up_and_triple_fires_once() {
        for modifier in [
            DoubleTapKey::Control,
            DoubleTapKey::Shift,
            DoubleTapKey::Alt,
            DoubleTapKey::Command,
        ] {
            for side in [KeySide::Left, KeySide::Right] {
                for configured in [KeySide::Any, side] {
                    let mut d = detector(modifier, configured);
                    let k = key(modifier, side);
                    assert!(!tap(&mut d, k, 0, 20));
                    assert!(tap(&mut d, k, 40, 60));
                    assert!(!tap(&mut d, k, 80, 100));
                }
            }
        }
    }

    #[test]
    fn hold_and_gap_boundaries_are_inclusive() {
        let k = key(DoubleTapKey::Control, KeySide::Left);
        let mut d = detector(k.key, KeySide::Any);
        assert!(!tap(&mut d, k, 0, 300));
        assert!(tap(&mut d, k, 600, 900));
        assert!(!tap(&mut d, k, 1000, 1301));
        assert!(!tap(&mut d, k, 1400, 1410));
        assert!(!tap(&mut d, k, 1711, 1720));
    }

    #[test]
    fn mixed_sides_and_unselected_sides_never_form_a_pair() {
        let left = key(DoubleTapKey::Control, KeySide::Left);
        let right = key(left.key, KeySide::Right);
        let mut d = detector(left.key, KeySide::Any);
        assert!(!tap(&mut d, left, 0, 10));
        assert!(!tap(&mut d, right, 20, 30));
        assert!(tap(&mut d, right, 40, 50));
        d.configure(DoubleTap {
            key: left.key,
            side: KeySide::Left,
        });
        assert!(!tap(&mut d, right, 60, 70));
        assert!(!tap(&mut d, right, 80, 90));
    }

    #[test]
    fn other_key_down_cancels_even_during_second_hold() {
        let k = key(DoubleTapKey::Control, KeySide::Left);
        let mut d = detector(k.key, KeySide::Any);
        assert!(!tap(&mut d, k, 0, 10));
        assert!(!d.modifier(k, true, 20));
        d.reset(); // Ctrl+C: C down before the second Ctrl up.
        assert!(!d.modifier(k, false, 30));
        assert!(!tap(&mut d, k, 40, 50));
        d.reset(); // Other key between taps.
        assert!(!tap(&mut d, k, 60, 70));
    }

    #[test]
    fn other_modifiers_and_overlapping_sides_cancel() {
        let k = key(DoubleTapKey::Control, KeySide::Left);
        for other in [
            key(DoubleTapKey::Shift, KeySide::Left),
            key(k.key, KeySide::Right),
        ] {
            let mut d = detector(k.key, KeySide::Any);
            assert!(!tap(&mut d, k, 0, 10));
            assert!(!d.modifier(k, true, 20));
            assert!(!d.modifier(other, true, 25));
            assert!(!d.modifier(other, false, 30));
            assert!(!d.modifier(k, false, 35));
            assert!(!tap(&mut d, k, 40, 50));
        }
    }

    #[test]
    fn repeats_are_ignored_and_cannot_restart_a_cancelled_hold() {
        let k = key(DoubleTapKey::Alt, KeySide::Right);
        let mut d = detector(k.key, KeySide::Any);
        assert!(!d.modifier(k, false, 0)); // unmatched up
        assert!(!d.modifier(k, true, 10));
        assert!(!d.modifier(k, true, 20));
        assert!(!d.modifier(k, false, 30));
        assert!(!d.modifier(k, true, 40));
        assert!(!d.modifier(k, true, 50));
        assert!(d.modifier(k, false, 60));
        assert!(!d.modifier(k, true, 70));
        d.reset();
        assert!(!d.modifier(k, true, 80));
        assert!(!d.modifier(k, false, 90));
        assert!(!tap(&mut d, k, 100, 110));
    }

    #[test]
    fn clear_released_drops_a_stuck_bit_so_the_next_tap_counts() {
        let ctrl = key(DoubleTapKey::Control, KeySide::Left);
        let alt = key(DoubleTapKey::Alt, KeySide::Left);
        let mut d = detector(ctrl.key, KeySide::Any);
        assert!(!d.modifier(alt, true, 0)); // Alt up never arrives.
        assert!(!tap(&mut d, ctrl, 10, 20));
        assert!(!tap(&mut d, ctrl, 30, 40));

        d.clear_released(|physical| physical != alt);
        assert!(!tap(&mut d, ctrl, 50, 60));
        assert!(tap(&mut d, ctrl, 70, 80));
    }

    #[test]
    fn disabled_and_reconfiguration_discard_gestures() {
        let k = key(DoubleTapKey::Shift, KeySide::Left);
        let mut d = detector(DoubleTapKey::Disabled, KeySide::Any);
        assert!(!tap(&mut d, k, 0, 10));
        assert!(!tap(&mut d, k, 20, 30));
        d.configure(DoubleTap {
            key: k.key,
            side: k.side,
        });
        assert!(!tap(&mut d, k, 40, 50));
        d.configure(DoubleTap::default());
        assert!(!tap(&mut d, k, 60, 70));
    }
}
