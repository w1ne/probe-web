//! Raspberry Pi microcontroller detection.
//!
//! Ported from probe-rs master (detection only; the fork has no RP debug sequences).
//! RP235x's CoreSight ROM table is Arm-designed (JEP106 Arm, part 1225), which many
//! Cortex-M33 chips share, so the chip is confirmed by reading SYSINFO.CHIP_ID.
use jep106::JEP106Code;
use probe_rs_target::Chip;

use crate::{
    architecture::arm::{
        ApV2Address, ArmChipInfo, ArmProbeInterface, FullyQualifiedApAddress, dp::DpAddress,
    },
    config::{DebugSequence, Registry},
    error::Error,
    vendor::Vendor,
};

/// Raspberry Pi
#[derive(docsplay::Display)]
pub struct RaspberryPi;

#[async_trait::async_trait(?Send)]
impl Vendor for RaspberryPi {
    fn try_create_debug_sequence(&self, _chip: &Chip) -> Option<DebugSequence> {
        None
    }

    async fn try_detect_arm_chip(
        &self,
        _registry: &Registry,
        interface: &mut dyn ArmProbeInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        const JEP_ARM: JEP106Code = JEP106Code { id: 0x3b, cc: 0x4 };
        const CHIPID_RP235X: u32 = 0x0000_4927;

        if chip_info.manufacturer != JEP_ARM || chip_info.part != 1225 {
            return Ok(None);
        }

        // RP235x exposes its SYSINFO block through the ADIv6 AP at 0x2000; on other
        // chips this AP does not exist and the access fails, which just means "not RP235x".
        let ap = FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address(Some(0x2000)));
        let Ok(mut memory) = interface.memory_interface(&ap).await else {
            return Ok(None);
        };
        match memory.read_word_32(0x4000_0000).await {
            Ok(chip_id) if (chip_id & 0x0fff_ffff) == CHIPID_RP235X => Ok(Some("RP235x".to_string())),
            _ => Ok(None),
        }
    }
}
