//! Text packets: Write Text (1, 8) and Write Special Symbols (2), ICD 2620001
//! Figure 3-8b (`docs/level3/reference.md` section 6.2).
//!
//! | Code | Fields after the code and length halfword |
//! |---|---|
//! | 1 | I, J, characters |
//! | 2 | I, J, special symbol characters |
//! | 8 | color level, I, J, characters |
//!
//! Coordinates are 1/4 km from the radar in the Product Symbology Block and
//! screen pixels in the Graphic Alphanumeric Block (section 3.3.3); the packet
//! itself does not say which.

use super::Packet;
use crate::Level3Error;
use crate::budget::Budget;

/// Text or special symbol packet (1, 2 or 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPacket {
    /// Packet code: 1 (Write Text, no value), 2 (Write Special Symbols) or 8
    /// (Write Text, uniform value).
    pub code: u16,
    /// Color level of the text, 0-15 (packet 8 only).
    pub color_level: Option<u16>,
    /// I coordinate: upper left corner of text, center of a special symbol
    /// (Figure 3-8b Note 1).
    pub i: i16,
    /// J coordinate, as for [`i`](Self::i).
    pub j: i16,
    /// The characters, one `char` per byte. The ICD defines them as ASCII;
    /// bytes 0x80-0xFF map to U+0080-U+00FF (ISO 8859-1), so `c as u8` gives
    /// back every byte. Observed: the supplemental text of products 32, 81 and
    /// 138 contains NUL characters, which are kept.
    pub text: String,
}

impl TextPacket {
    /// The packet code.
    pub fn code(&self) -> u16 {
        self.code
    }

    /// The special symbols of a packet 2 in character order, skipping spaces
    /// and characters the ICD does not assign (those stay visible in
    /// [`text`](Self::text)). Empty for packets 1 and 8.
    pub fn special_symbols(&self) -> impl Iterator<Item = SpecialSymbol> + '_ {
        let symbols = if self.code == 2 {
            self.text.as_str()
        } else {
            ""
        };
        symbols.chars().filter_map(SpecialSymbol::from_char)
    }
}

/// Special symbol characters of packet 2 (Figure 3-8b Note 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SpecialSymbol {
    /// `!`: past storm cell position.
    PastStormCell,
    /// `"`: current storm cell position.
    CurrentStormCell,
    /// `#`: forecast storm cell position.
    ForecastStormCell,
    /// `$`: past MDA (mesocyclone detection) position.
    PastMda,
    /// `%`: forecast MDA position.
    ForecastMda,
}

impl SpecialSymbol {
    /// The symbol a packet 2 character stands for; `None` for any other character.
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            '!' => Some(Self::PastStormCell),
            '"' => Some(Self::CurrentStormCell),
            '#' => Some(Self::ForecastStormCell),
            '$' => Some(Self::PastMda),
            '%' => Some(Self::ForecastMda),
            _ => None,
        }
    }

    /// The character that encodes this symbol.
    pub fn as_char(self) -> char {
        match self {
            Self::PastStormCell => '!',
            Self::CurrentStormCell => '"',
            Self::ForecastStormCell => '#',
            Self::PastMda => '$',
            Self::ForecastMda => '%',
        }
    }
}

/// Decodes one text packet (1, 2, 8). `bytes` is the complete packet, starting
/// with its 2-byte code, as sized by the dispatcher from its length halfword.
pub(crate) fn decode(code: u16, bytes: &[u8], budget: &mut Budget) -> Result<Packet, Level3Error> {
    // Byte offset of I: after the code and length, and the color level of packet 8.
    let i_at = match code {
        1 | 2 => 4,
        8 => 6,
        _ => return Err(Level3Error::UnsupportedPacket(code)),
    };
    let Some((head, characters)) = bytes.split_at_checked(i_at + 4) else {
        return Err(Level3Error::InvalidPacket {
            code,
            reason: format!(
                "length {} leaves no room for the {} bytes of fields before the text",
                bytes.len().saturating_sub(4),
                i_at
            ),
        });
    };
    let halfword = |at: usize| [head[at], head[at + 1]];
    Ok(Packet::Text(TextPacket {
        code,
        color_level: (code == 8).then(|| u16::from_be_bytes(halfword(4))),
        i: i16::from_be_bytes(halfword(i_at)),
        j: i16::from_be_bytes(halfword(i_at + 2)),
        text: budget.latin1(characters, "text packet")?,
    }))
}
