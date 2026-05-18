//! The Curator — a small turn counter that decides when the agent should
//! pause to extract memories from the recent conversation.
//!
//! Hermes calls this the "curator nudge": after the user has talked enough,
//! a system reminder is appended asking the model to write down anything
//! durable to its memory store. Without it, agents drift back to a stateless
//! mode; with it, the memory store grows naturally.
//!
//! The curator does not own memory itself — `merlion-memory` does. The CLI
//! holds the [`Curator`], calls [`Curator::record_user_turn`] for each user
//! input, and consults [`Curator::nudge_if_due`] to decide whether to
//! prepend a system reminder to the next prompt.

#[derive(Debug, Clone)]
pub struct Curator {
    interval: u32,
    turns_since_nudge: u32,
}

impl Curator {
    /// Build a curator that nudges every `interval` user turns. A value of 0
    /// disables nudging entirely.
    pub fn new(interval: u32) -> Self {
        Self { interval, turns_since_nudge: 0 }
    }

    pub fn interval(&self) -> u32 {
        self.interval
    }

    /// Call after each user message. Increments the internal counter.
    pub fn record_user_turn(&mut self) {
        self.turns_since_nudge = self.turns_since_nudge.saturating_add(1);
    }

    /// True when [`record_user_turn`] has fired `interval` times since the
    /// last reset. Always false when `interval == 0`.
    pub fn nudge_due(&self) -> bool {
        self.interval > 0 && self.turns_since_nudge >= self.interval
    }

    /// Return the nudge message and reset the counter. Returns `None` if no
    /// nudge is due. The returned text is suitable for prepending to the
    /// next user message as a `<system-reminder>` block.
    pub fn nudge_if_due(&mut self) -> Option<&'static str> {
        if !self.nudge_due() {
            return None;
        }
        self.turns_since_nudge = 0;
        Some(DEFAULT_NUDGE)
    }

    /// Reset the counter without emitting a nudge — e.g. after the model
    /// just saved a memory on its own initiative.
    pub fn reset(&mut self) {
        self.turns_since_nudge = 0;
    }
}

impl Default for Curator {
    /// Default cadence: nudge every 20 user turns.
    fn default() -> Self {
        Self::new(20)
    }
}

const DEFAULT_NUDGE: &str =
    "Take a moment to reflect on the recent conversation: are there any \
     facts about the user, their project, their preferences, or how they \
     like to work that would be valuable in a future session? If so, save \
     them to memory using the `memory` tool. Skip if there is nothing \
     worth keeping — quality over quantity.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudges_at_the_configured_interval() {
        let mut c = Curator::new(3);
        for _ in 0..2 {
            c.record_user_turn();
            assert!(c.nudge_if_due().is_none());
        }
        c.record_user_turn();
        assert!(c.nudge_if_due().is_some(), "should nudge on the 3rd turn");
        // counter should reset
        c.record_user_turn();
        assert!(c.nudge_if_due().is_none());
    }

    #[test]
    fn interval_zero_disables_nudging() {
        let mut c = Curator::new(0);
        for _ in 0..1000 {
            c.record_user_turn();
        }
        assert!(c.nudge_if_due().is_none());
    }

    #[test]
    fn reset_clears_counter_without_returning_nudge() {
        let mut c = Curator::new(2);
        c.record_user_turn();
        c.record_user_turn();
        assert!(c.nudge_due());
        c.reset();
        assert!(!c.nudge_due());
    }
}
