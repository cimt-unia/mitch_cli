use lsl::{StreamInfo, StreamOutlet};
use serde::Serialize;

pub struct MyInfo(pub StreamInfo);
unsafe impl Send for MyInfo {}
impl MyInfo {
    pub fn from_info(info: StreamInfo) -> Self {
        Self(info)
    }
}

pub struct MyOutlet(pub StreamOutlet);
unsafe impl Send for MyOutlet {}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum MitchState {
    SysStartup = 0x01,
    SysIdle = 0x02,
    SysStandby = 0x03,
    SysLog = 0x04,
    SysReadout = 0x05,
    SysTx = 0xF8,
    SysError = 0xFF,
    BootStartup = 0xf0,
    BootIdle = 0xf1,
    BootDownload = 0xf2,
}

impl TryFrom<u8> for MitchState {
    type Error = &'static str;

    fn try_from(value: u8) -> std::result::Result<MitchState, &'static str> {
        if (1_u8..=5_u8).contains(&value)
            || value == 0xf8
            || value == 0xff
            || value == 0xf0
            || value == 0xf1
            || value == 0xf2
        {
            return Ok(unsafe { *(&value as *const _ as *const MitchState) });
        }
        Err("Unknown state")
    }
}

pub enum Commands {
    GetState,
    GetPower,
    StartAccelerometryStream,
    StartPressureStream,
    StopStream,
}

impl AsRef<[u8]> for Commands {
    fn as_ref(&self) -> &[u8] {
        match self {
            Commands::GetState => &[130, 0],
            Commands::StartAccelerometryStream => &[0x02, 0x03, 0xF8, 0x04, 0x04],
            Commands::StartPressureStream => &[0x02, 0x03, 0xF8, 0x01, 0x04],
            Commands::StopStream => &[0x02, 0x01, 0x02],
            Commands::GetPower => &[87, 0],
        }
    }
}
