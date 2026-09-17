//! Volume Coverage Pattern (messages 5 and 7, ICD 2620002AA Table XI).
//!
//! Placeholder: the typed decoder lands in wave 2 task A.2. Until then
//! [`MessageWalker`](super::MessageWalker) yields these bodies as
//! [`MessageBody::Unparsed`].

use std::borrow::Cow;

use super::MessageBody;
use crate::Result;

/// Decoded Volume Coverage Pattern (Table XI). Fields are added with the decoder.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct VolumeCoveragePattern {}

/// Walker hook: the typed body for messages 5 and 7.
pub(crate) fn message_body(body: Cow<'_, [u8]>) -> Result<MessageBody<'_>> {
    Ok(MessageBody::Unparsed(body))
}
