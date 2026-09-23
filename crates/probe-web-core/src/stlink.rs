//! Classify an ST-Link open failure that means the debug interface was not claimed.
//! CMSIS-DAP v1 (HID) stays `unsupported`; this kind is only the interface walk.

const STLINK_VID: u16 = 0x0483;

pub(crate) fn open_failure_kind(vendor_id: u16, message: &str) -> &'static str {
    if vendor_id == STLINK_VID && stlink_interface_failure(message) {
        "stlink-interface"
    } else {
        "open-failed"
    }
}

fn stlink_interface_failure(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    // nusb's wasm backend replaces the DOMException with "WebUSB error (see logs)".
    // "interface already claimed" is the busy claim. The other two are the browser's
    // claimInterface text when it is still visible.
    message.contains("endpoint not found")
        || message.contains("endpointnotfound")
        || message.contains("interface not found")
        || message.contains("claiminterface")
        || message.contains("could not be claimed")
        || message.contains("webusb error (see logs)")
        || message.contains("interface already claimed")
        || message.contains("failed to claim interface")
        || message.contains("unable to claim interface")
}

#[cfg(test)]
mod test {
    use super::open_failure_kind;

    #[test]
    fn endpoint_not_found_on_stlink_is_stlink_interface() {
        let message = "The debug probe could not be created. (ProbeCouldNotBeCreated(ProbeSpecific(EndpointNotFound)))";
        assert_eq!(open_failure_kind(0x0483, message), "stlink-interface");
    }

    #[test]
    fn hid_copy_is_not_reused_for_other_vendors() {
        assert_eq!(
            open_failure_kind(0x0d28, "USB endpoint not found"),
            "open-failed"
        );
    }

    #[test]
    fn unrelated_stlink_open_failure_stays_open_failed() {
        assert_eq!(
            open_failure_kind(0x0483, "permission denied"),
            "open-failed"
        );
    }

    #[test]
    fn webusb_error_on_stlink_is_stlink_interface() {
        assert_eq!(
            open_failure_kind(0x0483, "WebUSB error (see logs)"),
            "stlink-interface"
        );
    }

    #[test]
    fn webusb_error_on_daplink_stays_open_failed() {
        assert_eq!(
            open_failure_kind(0x0d28, "WebUSB error (see logs)"),
            "open-failed"
        );
    }
}
