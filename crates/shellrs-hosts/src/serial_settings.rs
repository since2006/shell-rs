//! How a serial host's line is set. The device itself is the host's
//! `address`.

/// The baud rates the host form offers. Any other rate can be typed.
pub const COMMON_BAUD_RATES: [u32; 12] = [
    1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1500000,
];

/// How a serial line is set: what both ends must agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialSettings {
    pub baud: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow: FlowControl,
}

/// 115200 8N1 without flow control, what most consoles use today.
impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud: 115200,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow: FlowControl::None,
        }
    }
}

impl SerialSettings {
    /// `115200 8N1`, the way a serial line is written down.
    pub fn summary(&self) -> String {
        format!(
            "{} {}{}{}",
            self.baud,
            self.data_bits.value(),
            self.parity.letter(),
            self.stop_bits.value()
        )
    }
}

/// Bits per character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataBits {
    Five,
    Six,
    Seven,
    Eight,
}

impl DataBits {
    /// Every size, in the order the form lists them.
    pub const ALL: [DataBits; 4] = [
        DataBits::Five,
        DataBits::Six,
        DataBits::Seven,
        DataBits::Eight,
    ];

    /// The number, also what is stored.
    pub fn value(self) -> u8 {
        match self {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        }
    }

    pub fn from_value(value: i64) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|bits| i64::from(bits.value()) == value)
    }
}

/// The parity bit after each character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parity {
    None,
    Odd,
    Even,
}

impl Parity {
    /// Every parity, in the order the form lists them.
    pub const ALL: [Parity; 3] = [Parity::None, Parity::Odd, Parity::Even];

    /// The stored spelling, also the external CLI's.
    pub fn as_str(self) -> &'static str {
        match self {
            Parity::None => "none",
            Parity::Odd => "odd",
            Parity::Even => "even",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|parity| parity.as_str() == value)
    }

    /// The letter of `8N1`.
    fn letter(self) -> char {
        match self {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        }
    }
}

/// Stop bits after each character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopBits {
    One,
    Two,
}

impl StopBits {
    /// Every count, in the order the form lists them.
    pub const ALL: [StopBits; 2] = [StopBits::One, StopBits::Two];

    /// The number, also what is stored.
    pub fn value(self) -> u8 {
        match self {
            StopBits::One => 1,
            StopBits::Two => 2,
        }
    }

    pub fn from_value(value: i64) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|bits| i64::from(bits.value()) == value)
    }
}

/// How either end tells the other to pause. DSR/DTR is not offered: the
/// termios of macOS and Linux has no such mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowControl {
    None,
    /// Hardware flow control, over the RTS and CTS lines.
    RtsCts,
    /// Software flow control, by sending ^S and ^Q.
    XonXoff,
}

impl FlowControl {
    /// Every mode, in the order the form lists them.
    pub const ALL: [FlowControl; 3] =
        [FlowControl::None, FlowControl::RtsCts, FlowControl::XonXoff];

    /// The stored spelling, also the external CLI's.
    pub fn as_str(self) -> &'static str {
        match self {
            FlowControl::None => "none",
            FlowControl::RtsCts => "rts-cts",
            FlowControl::XonXoff => "xon-xoff",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|flow| flow.as_str() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_written_down_as_rate_and_frame() {
        assert_eq!(SerialSettings::default().summary(), "115200 8N1");
        let settings = SerialSettings {
            baud: 9600,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            flow: FlowControl::RtsCts,
        };
        assert_eq!(settings.summary(), "9600 7E2");
    }

    #[test]
    fn every_setting_round_trips_through_its_stored_spelling() {
        for bits in DataBits::ALL {
            assert_eq!(DataBits::from_value(bits.value().into()), Some(bits));
        }
        for parity in Parity::ALL {
            assert_eq!(Parity::from_stored(parity.as_str()), Some(parity));
        }
        for bits in StopBits::ALL {
            assert_eq!(StopBits::from_value(bits.value().into()), Some(bits));
        }
        for flow in FlowControl::ALL {
            assert_eq!(FlowControl::from_stored(flow.as_str()), Some(flow));
        }
        assert_eq!(DataBits::from_value(9), None);
        assert_eq!(Parity::from_stored("mark"), None);
        assert_eq!(FlowControl::from_stored("dsr-dtr"), None);
    }
}
