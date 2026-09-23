use nusb::DeviceInfo;
use std::{sync::LazyLock, time::Duration};

use crate::probe::{stlink::StlinkError, usb_util::InterfaceExt};

use std::collections::HashMap;

use super::tools::{is_stlink_device, read_serial_number};
use crate::probe::{DebugProbeSelector, ProbeCreationError};

/// USB mass-storage class. Interface 0 on a composite Nucleo is this, and
/// WebUSB must not be asked to claim it.
const USB_CLASS_MASS_STORAGE: u8 = 0x08;

/// The USB Command packet size.
const CMD_LEN: usize = 16;

/// The USB VendorID.
pub const USB_VID: u16 = 0x0483;

pub const TIMEOUT: Duration = Duration::from_millis(1000);

/// Map of USB PID to firmware version name and device endpoints.
pub static USB_PID_EP_MAP: LazyLock<HashMap<u16, StLinkInfo>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert(0x3748, StLinkInfo::new("V2", 0x02, 0x81, 0x83));
    m.insert(0x374b, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82));
    m.insert(0x374a, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82)); // Audio
    m.insert(0x3742, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82)); // No MSD
    m.insert(0x3752, StLinkInfo::new("V2-1", 0x01, 0x81, 0x82)); // Unproven
    m.insert(0x374e, StLinkInfo::new("V3", 0x01, 0x81, 0x82));
    m.insert(0x374f, StLinkInfo::new("V3", 0x01, 0x81, 0x82)); // Bridge
    m.insert(0x3753, StLinkInfo::new("V3", 0x01, 0x81, 0x82)); // 2VCP
    m.insert(0x3754, StLinkInfo::new("V3", 0x01, 0x81, 0x82)); // Without mass storage
    m.insert(0x3757, StLinkInfo::new("V3PWR", 0x01, 0x81, 0x82)); // Bridge and power, no MSD
    m
});

/// A helper struct to match STLink device info.
#[derive(Clone, Debug, Default)]
pub struct StLinkInfo {
    pub version_name: &'static str,
    ep_out: u8,
    ep_in: u8,
    ep_swo: u8,
}

impl StLinkInfo {
    pub const fn new(version_name: &'static str, ep_out: u8, ep_in: u8, ep_swo: u8) -> Self {
        Self {
            version_name,
            ep_out,
            ep_in,
            ep_swo,
        }
    }
}

/// One alternate setting seen while walking a configuration.
struct StLinkAltSetting {
    interface_number: u8,
    class: u8,
    endpoints: Vec<u8>,
}

/// Interface number whose alternate setting contains the PID's endpoints.
///
/// Mass storage (class 0x08) is skipped even if the addresses collide: claiming
/// it is the composite-Nucleo failure. `None` means do not fall back to interface 0.
fn stlink_debug_interface_number(
    alt_settings: &[StLinkAltSetting],
    info: &StLinkInfo,
) -> Option<u8> {
    alt_settings.iter().find_map(|alt| {
        if alt.class == USB_CLASS_MASS_STORAGE {
            return None;
        }
        let has_debug_endpoints = alt.endpoints.contains(&info.ep_out)
            && alt.endpoints.contains(&info.ep_in)
            && alt.endpoints.contains(&info.ep_swo);
        has_debug_endpoints.then_some(alt.interface_number)
    })
}

pub(crate) struct StLinkUsbDevice {
    device_handle: nusb::Device,
    interface: nusb::Interface,
    pub(crate) info: StLinkInfo,
}

impl std::fmt::Debug for StLinkUsbDevice {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("StLinkUsbDevice")
            .field("device_handle", &"DeviceHandle<rusb::Context>")
            .field("info", &self.info)
            .finish()
    }
}

#[async_trait::async_trait(?Send)]
pub trait StLinkUsb: std::fmt::Debug {
    /// Writes to the probe and reads back data if needed.
    async fn write(
        &mut self,
        cmd: &[u8],
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), StlinkError>;

    /// Reset the USB device. This can be used to recover when the
    /// STLink does not respond to USB requests.
    async fn reset(&mut self) -> Result<(), StlinkError>;

    /// Reads SWO data from the probe.
    async fn read_swo(
        &mut self,
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, StlinkError>;
}

// Copy of `Selector::matches` except it uses the stlink-specific read_serial_number
// to handle the broken stlink-v2 serial numbers that need hex-encoding.
fn selector_matches(selector: &DebugProbeSelector, info: &DeviceInfo) -> bool {
    info.vendor_id() == selector.vendor_id
        && info.product_id() == selector.product_id
        && selector
            .serial_number
            .as_ref()
            .map(|s| {
                if let Some(serial) = read_serial_number(info) {
                    serial.as_str() == s
                } else {
                    s.is_empty()
                }
            })
            .unwrap_or(true)
}

impl StLinkUsbDevice {
    /// Creates and initializes a new USB device.
    pub async fn new_from_selector(
        selector: &DebugProbeSelector,
    ) -> Result<Self, ProbeCreationError> {
        let device = crate::probe::list::list_devices()
            .await
            .map_err(|e| ProbeCreationError::Usb(e.into()))?
            .filter(is_stlink_device)
            .find(|device| selector_matches(selector, device))
            .ok_or(ProbeCreationError::NotFound)?;

        let info = USB_PID_EP_MAP[&device.product_id()].clone();

        let device_handle = device
            .open()
            .await
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        tracing::debug!("Aquired handle for probe");

        let config = device_handle.configurations().next().unwrap();
        // Every interface, not interfaces().next(): on V2-1/V3 the debug
        // endpoints are not on interface 0.
        let alt_settings: Vec<StLinkAltSetting> = config
            .interfaces()
            .flat_map(|interface| {
                let interface_number = interface.interface_number();
                // Collect before the interface borrow ends; descriptors do not outlive it.
                interface
                    .alt_settings()
                    .map(|descriptor| StLinkAltSetting {
                        interface_number,
                        class: descriptor.class(),
                        endpoints: descriptor
                            .endpoints()
                            .map(|endpoint| endpoint.address())
                            .collect(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        let Some(interface_number) = stlink_debug_interface_number(&alt_settings, &info) else {
            return Err(StlinkError::EndpointNotFound.into());
        };

        let interface = device_handle
            .claim_interface(interface_number)
            .await
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        tracing::debug!("Claimed interface {interface_number} of USB device.");

        let usb_stlink = Self {
            device_handle,
            interface,
            info,
        };

        tracing::debug!("Succesfully attached to STLink.");

        Ok(usb_stlink)
    }
}

#[async_trait::async_trait(?Send)]
impl StLinkUsb for StLinkUsbDevice {
    /// Writes to the out EP and reads back data if needed.
    /// First the `cmd` is sent.
    /// In a second step `write_data` is transmitted.
    /// And lastly, data will be read back until `read_data` is filled.
    async fn write(
        &mut self,
        cmd: &[u8],
        write_data: &[u8],
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<(), StlinkError> {
        tracing::trace!(
            "Sending command {:x?} to STLink, timeout: {:?}",
            cmd,
            timeout
        );

        // Command phase.
        assert!(cmd.len() <= CMD_LEN);
        let mut padded_cmd = [0u8; CMD_LEN];
        padded_cmd[..cmd.len()].copy_from_slice(cmd);

        let ep_out = self.info.ep_out;
        let ep_in = self.info.ep_in;

        let written_bytes = self
            .interface
            .write_bulk(ep_out, &padded_cmd, timeout)
            .await?;

        if written_bytes != CMD_LEN {
            return Err(StlinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: CMD_LEN,
            });
        }

        // Optional data out phase.
        if !write_data.is_empty() {
            let mut remaining_bytes = write_data.len();

            let mut write_index = 0;

            while remaining_bytes > 0 {
                let written_bytes = self
                    .interface
                    .write_bulk(ep_out, &write_data[write_index..], timeout)
                    .await?;

                remaining_bytes -= written_bytes;
                write_index += written_bytes;

                tracing::trace!(
                    "Wrote {} bytes, {} bytes remaining",
                    written_bytes,
                    remaining_bytes
                );
            }

            tracing::trace!("USB write done!");
        }

        // Optional data in phase.
        if !read_data.is_empty() {
            let mut remaining_bytes = read_data.len();
            let mut read_index = 0;

            while remaining_bytes > 0 {
                let read_bytes = self
                    .interface
                    .read_bulk(ep_in, &mut read_data[read_index..], timeout)
                    .await?;

                read_index += read_bytes;
                remaining_bytes -= read_bytes;

                tracing::trace!(
                    "Read {} bytes, {} bytes remaining",
                    read_bytes,
                    remaining_bytes
                );
            }
        }
        Ok(())
    }

    async fn read_swo(
        &mut self,
        read_data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, StlinkError> {
        tracing::trace!(
            "Reading {:?} SWO bytes to STLink, timeout: {:?}",
            read_data.len(),
            timeout
        );

        let ep_swo = self.info.ep_swo;

        if read_data.is_empty() {
            Ok(0)
        } else {
            self.interface
                .read_bulk(ep_swo, read_data, timeout)
                .await
                .map_err(|e| StlinkError::Usb(e.into()))
        }
    }

    /// Reset the USB device. This can be used to recover when the
    /// STLink does not respond to USB requests.
    async fn reset(&mut self) -> Result<(), StlinkError> {
        tracing::debug!("Resetting USB device of STLink");
        self.device_handle
            .reset()
            .await
            .map_err(|e| StlinkError::Usb(e.into()))
    }
}

#[cfg(test)]
mod test {
    use super::{
        StLinkAltSetting, USB_CLASS_MASS_STORAGE, USB_PID_EP_MAP, stlink_debug_interface_number,
    };

    fn alt(interface_number: u8, class: u8, endpoints: &[u8]) -> StLinkAltSetting {
        StLinkAltSetting {
            interface_number,
            class,
            endpoints: endpoints.to_vec(),
        }
    }

    #[test]
    fn composite_v2_1_claims_debug_interface_not_mass_storage() {
        let info = &USB_PID_EP_MAP[&0x374b];
        // Interface 0 is mass storage and shares two of the V2-1 addresses.
        // The debug interface is the later one that has all three.
        let config = [
            alt(0, USB_CLASS_MASS_STORAGE, &[0x81, info.ep_out]),
            alt(1, 0x0a, &[0x82, 0x02]),
            alt(3, 0xff, &[info.ep_out, info.ep_in, info.ep_swo]),
        ];

        let interface_number = stlink_debug_interface_number(&config, info);

        assert_eq!(interface_number, Some(3));
        assert_ne!(interface_number, Some(0));
    }

    #[test]
    fn single_interface_v2_claims_interface_zero() {
        let info = &USB_PID_EP_MAP[&0x3748];
        let config = [alt(0, 0xff, &[info.ep_out, info.ep_in, info.ep_swo])];

        assert_eq!(stlink_debug_interface_number(&config, info), Some(0));
    }

    #[test]
    fn mass_storage_alone_is_not_claimed() {
        let info = &USB_PID_EP_MAP[&0x374b];
        // Even if the addresses were copied onto class 0x08, do not claim it.
        let config = [alt(
            0,
            USB_CLASS_MASS_STORAGE,
            &[info.ep_out, info.ep_in, info.ep_swo],
        )];

        assert_eq!(stlink_debug_interface_number(&config, info), None);
    }
}
