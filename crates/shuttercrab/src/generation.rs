//! Telling the latest of several overlapping starts from those before it.
//! A timer started by an earlier start checks its token when it fires, and
//! does nothing if a later start has replaced it.

use std::fmt;

/// Counts the starts of something, such as Copy's check mark.
#[derive(Debug, Default)]
pub struct Generation(u64);

/// One start of a [`Generation`]: current until the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token(u64);

impl Generation {
    /// Start again: every token handed out before stops being current.
    pub fn start(&mut self) -> Token {
        self.0 += 1;
        Token(self.0)
    }

    /// The latest start's token, for something that begins later but must
    /// not outlive the next start.
    pub fn current(&self) -> Token {
        Token(self.0)
    }

    /// Whether `token` is from the latest start.
    pub fn is_current(&self, token: Token) -> bool {
        self.0 == token.0
    }
}

/// A token shows as its number, for ids that must change with each start,
/// such as an animation's.
impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_token_is_current() {
        let mut generation = Generation::default();
        let first = generation.start();
        assert!(generation.is_current(first));
        let second = generation.start();
        assert!(!generation.is_current(first));
        assert!(generation.is_current(second));
        assert_eq!(generation.current(), second);
        assert_ne!(first.to_string(), second.to_string());
    }
}
