use serde::Serialize;

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
        Ok(match value {
            0x01 => MitchState::SysStartup,
            0x02 => MitchState::SysIdle,
            0x03 => MitchState::SysStandby,
            0x04 => MitchState::SysLog,
            0x05 => MitchState::SysReadout,
            0xF8 => MitchState::SysTx,
            0xFF => MitchState::SysError,
            0xF0 => MitchState::BootStartup,
            0xF1 => MitchState::BootIdle,
            0xF2 => MitchState::BootDownload,
            _ => return Err("Unknown state"),
        })
    }
}

#[allow(dead_code)]
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
            Commands::GetPower => &[0x87, 0],
        }
    }
}
