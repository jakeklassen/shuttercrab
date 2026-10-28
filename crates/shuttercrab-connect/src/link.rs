//! The link a stream is shared by: `shuttercrab://watch/<room>#<secret>`.
//!
//! The room is all the meeting point sees. The secret, after the `#`,
//! stays between the two apps: the fingerprint of the certificate the
//! streaming app will show, so the viewer knows it reached the right app,
//! and a token the viewer shows it, so only those with the link watch.

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as BASE64};
use std::fmt;

const PREFIX: &str = "shuttercrab://watch/";

#[derive(Clone, PartialEq, Eq)]
pub struct Link {
    /// Which room on the meeting point.
    pub room: [u8; 16],
    /// SHA-256 of the streaming app's certificate.
    pub fingerprint: [u8; 32],
    /// What a viewer shows the streaming app to be let in.
    pub token: [u8; 16],
}

impl Link {
    /// The room's name in the meeting point's URL.
    pub fn room_name(&self) -> String {
        BASE64.encode(self.room)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let rest = text
            .trim()
            .strip_prefix(PREFIX)
            .with_context(|| format!("a link starts {PREFIX}"))?;
        let (room, secret) = rest
            .split_once('#')
            .context("the link is missing the part after #")?;
        let room = BASE64.decode(room).context("the link's room is garbled")?;
        let secret = BASE64
            .decode(secret)
            .context("the part after # is garbled")?;
        ensure!(room.len() == 16, "the link's room is the wrong length");
        if secret.len() != 48 {
            bail!("the part after # is the wrong length");
        }
        Ok(Self {
            room: room.try_into().expect("16 bytes"),
            fingerprint: secret[..32].try_into().expect("32 bytes"),
            token: secret[32..].try_into().expect("16 bytes"),
        })
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secret = [&self.fingerprint[..], &self.token[..]].concat();
        write!(f, "{PREFIX}{}#{}", self.room_name(), BASE64.encode(secret))
    }
}

/// Without the secret: a link may end up in a log.
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}#…", self.room_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_reads_back_as_it_was_written() {
        let link = Link {
            room: [1; 16],
            fingerprint: [2; 32],
            token: [3; 16],
        };
        let text = link.to_string();
        assert!(text.starts_with(PREFIX));
        assert_eq!(Link::parse(&text).unwrap(), link);
        assert!(!format!("{link:?}").contains(&text[text.find('#').unwrap() + 1..]));
    }

    #[test]
    fn a_damaged_link_is_refused() {
        assert!(Link::parse("https://example.com").is_err());
        assert!(Link::parse("shuttercrab://watch/AAAA").is_err());
        assert!(Link::parse("shuttercrab://watch/AAAAAAAAAAAAAAAAAAAAAA#AAAA").is_err());
    }
}
