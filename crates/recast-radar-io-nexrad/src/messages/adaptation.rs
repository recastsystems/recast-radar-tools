//! RDA Adaptation Data (message 18, ICD 2620002AA Table XV).
//!
//! Placeholder: the typed decoder lands in wave 2 task A.2. Until then
//! [`MessageWalker`](super::MessageWalker) yields these bodies as
//! [`MessageBody::Unparsed`].

use std::borrow::Cow;

use super::MessageBody;
use crate::Result;

/// Decoded RDA Adaptation Data (Table XV). Fields are added with the decoder.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RdaAdaptationData {}

/// Walker hook: the typed body for message 18.
pub(crate) fn message_body(body: Cow<'_, [u8]>) -> Result<MessageBody<'_>> {
    Ok(MessageBody::Unparsed(body))
}
