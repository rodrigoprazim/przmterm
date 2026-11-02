//! Serial line parameters, shared by session files and the serial transport.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const DEFAULT_BAUD_RATE: u32 = 9600;
pub const DEFAULT_DATA_BITS: u8 = 8;
pub const DEFAULT_STOP_BITS: u8 = 1;

/// Baud rates offered in pickers; any other rate can be typed.
pub const COMMON_BAUD_RATES: [u32; 10] = [
    1200, 2400, 4800, 9600, 19200, 38400, 57600, 115_200, 230_400, 460_800,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

impl Parity {
    pub const ALL: [Parity; 3] = [Parity::None, Parity::Odd, Parity::Even];

    /// Lower-case name, as written in session files.
    pub fn name(self) -> &'static str {
        match self {
            Parity::None => "none",
            Parity::Odd => "odd",
            Parity::Even => "even",
        }
    }

    /// The letter in `8N1`.
    pub fn letter(self) -> char {
        match self {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        }
    }

    pub fn from_name(name: &str) -> Option<Parity> {
        Parity::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowControl {
    #[default]
    None,
    /// XON/XOFF.
    Software,
    /// RTS/CTS.
    Hardware,
}

impl FlowControl {
    pub const ALL: [FlowControl; 3] = [
        FlowControl::None,
        FlowControl::Software,
        FlowControl::Hardware,
    ];

    /// Lower-case name, as written in session files.
    pub fn name(self) -> &'static str {
        match self {
            FlowControl::None => "none",
            FlowControl::Software => "software",
            FlowControl::Hardware => "hardware",
        }
    }

    /// Name shown in the GUI.
    pub fn label(self) -> &'static str {
        match self {
            FlowControl::None => "none",
            FlowControl::Software => "XON/XOFF",
            FlowControl::Hardware => "RTS/CTS",
        }
    }

    pub fn from_name(name: &str) -> Option<FlowControl> {
        FlowControl::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// Everything needed to open a serial port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialLine {
    /// `/dev/ttyUSB0`, `/dev/cu.usbserial-1410`, `COM3`…
    pub port: String,
    pub baud_rate: u32,
    /// 5 to 8.
    pub data_bits: u8,
    pub parity: Parity,
    /// 1 or 2.
    pub stop_bits: u8,
    pub flow_control: FlowControl,
}

impl SerialLine {
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            baud_rate: DEFAULT_BAUD_RATE,
            data_bits: DEFAULT_DATA_BITS,
            parity: Parity::None,
            stop_bits: DEFAULT_STOP_BITS,
            flow_control: FlowControl::None,
        }
    }

    /// The frame in the usual short form, e.g. `8N1`.
    pub fn frame(&self) -> String {
        format!(
            "{}{}{}",
            self.data_bits,
            self.parity.letter(),
            self.stop_bits
        )
    }
}

impl fmt::Display for SerialLine {
    /// `/dev/ttyUSB0 9600 8N1`, plus the flow control when there is one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.port, self.baud_rate, self.frame())?;
        if self.flow_control != FlowControl::None {
            write!(f, " {}", self.flow_control.label())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_line() {
        let mut line = SerialLine::new("COM3");
        assert_eq!(line.to_string(), "COM3 9600 8N1");
        line.parity = Parity::Even;
        line.data_bits = 7;
        line.flow_control = FlowControl::Hardware;
        assert_eq!(line.to_string(), "COM3 9600 7E1 RTS/CTS");
        assert_eq!(Parity::from_name("odd"), Some(Parity::Odd));
        assert_eq!(
            FlowControl::from_name("software"),
            Some(FlowControl::Software)
        );
    }
}
