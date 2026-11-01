//! What's plugged into the Mac or connected to it, as System Information lists it.

use std::process::Command;

use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Usb,
    Thunderbolt,
    Bluetooth,
    Display,
}

#[derive(Debug, PartialEq)]
pub struct Device {
    pub kind: Kind,
    pub name: String,
    /// Like who makes it, or how it's shown.
    pub detail: String,
}

/// What's connected, from `system_profiler`, which takes a moment.
pub fn connected() -> Vec<Device> {
    let output = Command::new("/usr/sbin/system_profiler")
        .args(["-json", "-detailLevel", "mini"])
        .args(["SPDisplaysDataType", "SPThunderboltDataType", "SPUSBHostDataType", "SPBluetoothDataType"])
        .output();
    match output {
        Ok(output) => serde_json::from_slice(&output.stdout).map(|report| parse(&report)).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn parse(report: &Value) -> Vec<Device> {
    let mut devices = Vec::new();
    let list = |key| report[key].as_array().into_iter().flatten();
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    for gpu in list("SPDisplaysDataType") {
        for display in gpu["spdisplays_ndrvs"].as_array().into_iter().flatten() {
            let resolution = display["_spdisplays_resolution"].as_str().or(display["spdisplays_resolution"].as_str());
            devices.push(Device {
                kind: Kind::Display,
                name: text(display, "_name"),
                detail: resolution.unwrap_or_default().replace(" x ", " × "),
            });
        }
    }
    // Each bus is one of the Mac's own ports, with what's plugged into it below.
    for bus in list("SPThunderboltDataType") {
        walk(bus, &mut |device| {
            devices.push(Device {
                kind: Kind::Thunderbolt,
                name: text(device, "device_name_key"),
                detail: text(device, "vendor_name_key"),
            })
        });
    }
    for bus in list("SPUSBHostDataType") {
        walk(bus, &mut |device| {
            // Like the hubs in the Mac itself.
            if device["USBKeyHardwareType"] != "Built-in" {
                devices.push(Device {
                    kind: Kind::Usb,
                    name: text(device, "_name"),
                    detail: text(device, "USBDeviceKeyVendorName"),
                });
            }
        });
    }
    for controller in list("SPBluetoothDataType") {
        for connected in controller["device_connected"].as_array().into_iter().flatten() {
            for (name, device) in connected.as_object().into_iter().flatten() {
                let battery = device["device_batteryLevelMain"].as_str();
                let detail = [device["device_minorType"].as_str(), battery].into_iter().flatten();
                devices.push(Device {
                    kind: Kind::Bluetooth,
                    name: name.clone(),
                    detail: detail.collect::<Vec<_>>().join(", "),
                });
            }
        }
    }
    devices
}

/// Calls `found` with everything below `entry`, however deep, as hubs and daisy chains go.
fn walk(entry: &Value, found: &mut impl FnMut(&Value)) {
    for item in entry["_items"].as_array().into_iter().flatten() {
        found(item);
        walk(item, found);
    }
}

#[cfg(test)]
mod tests {
    use super::{Device, Kind, parse};

    #[test]
    fn lists_what_is_connected() {
        let report = serde_json::json!({
            "SPDisplaysDataType": [{"_name": "Apple M4", "spdisplays_ndrvs": [
                {"_name": "Studio Display", "_spdisplays_resolution": "2560 x 1440 @ 60.00Hz"}
            ]}],
            "SPThunderboltDataType": [{"_name": "thunderboltusb4_bus_0", "device_name_key": "Mac mini", "_items": [
                {"device_name_key": "TS4", "vendor_name_key": "CalDigit"}
            ]}],
            "SPUSBHostDataType": [{"_name": "USB 3.1 Bus", "USBKeyHardwareType": "Built-in", "_items": [
                {"_name": "USB2 Hub", "USBKeyHardwareType": "Built-in", "_items": [
                    {"_name": "Portable SSD T5", "USBDeviceKeyVendorName": "Samsung", "USBKeyHardwareType": "Removable"}
                ]}
            ]}],
            "SPBluetoothDataType": [{
                "device_connected": [{"Magic Keyboard": {"device_minorType": "Keyboard", "device_batteryLevelMain": "80%"}}],
                "device_not_connected": [{"AirPods": {}}]
            }]
        });
        let device = |kind, name: &str, detail: &str| Device { kind, name: name.into(), detail: detail.into() };
        assert_eq!(
            parse(&report),
            [
                device(Kind::Display, "Studio Display", "2560 × 1440 @ 60.00Hz"),
                device(Kind::Thunderbolt, "TS4", "CalDigit"),
                device(Kind::Usb, "Portable SSD T5", "Samsung"),
                device(Kind::Bluetooth, "Magic Keyboard", "Keyboard, 80%"),
            ]
        );
    }
}
