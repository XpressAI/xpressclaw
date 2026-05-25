//! Android device control via the `adb_client` crate — pure Rust, no `adb`
//! binary required. Gated behind the `android` Cargo feature (off by default).
//!
//! The control layer is **provider-agnostic**: it speaks the adb wire protocol
//! to whatever `adbd` is reachable, whether that's a managed emulator
//! (`via_server` / `via_tcp`) or a BYO real device. Only provisioning differs
//! between providers — control is shared. See ADR-024.
//!
//! Element targeting uses the `uiautomator` accessibility tree
//! ([`AndroidDevice::find_element`]) rather than vision-derived pixel
//! coordinates, which are unreliable (see ADR-024).

use std::net::SocketAddr;

use adb_client::{server::ADBServer, tcp::ADBTcpDevice, ADBDeviceExt};

use crate::error::{Error, Result};

/// A resolved on-screen UI element's bounds, in device pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiElement {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl UiElement {
    /// Center point — the reliable tap target.
    pub fn center(&self) -> (i32, i32) {
        ((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }
}

/// A connected Android device, controlled over the adb wire protocol.
pub struct AndroidDevice {
    inner: Box<dyn ADBDeviceExt>,
}

impl AndroidDevice {
    /// Connect through the local adb server (default `127.0.0.1:5037`) to a
    /// device by serial, e.g. `"emulator-5554"`.
    pub fn via_server(serial: &str) -> Result<Self> {
        let mut server = ADBServer::default();
        let device = server.get_device_by_name(serial).map_err(|e| {
            Error::Android(format!("adb server connect to '{serial}' failed: {e}"))
        })?;
        Ok(Self {
            inner: Box::new(device),
        })
    }

    /// Connect directly to a device's `adbd` over TCP — no adb server, no adb
    /// binary. For the managed emulator this is `127.0.0.1:5555`.
    pub fn via_tcp(addr: SocketAddr) -> Result<Self> {
        let device = ADBTcpDevice::new(addr)
            .map_err(|e| Error::Android(format!("direct tcp connect to {addr} failed: {e}")))?;
        Ok(Self {
            inner: Box::new(device),
        })
    }

    /// Run a shell command and capture stdout as a string.
    pub fn shell(&mut self, command: &str) -> Result<String> {
        let mut out = Vec::new();
        self.inner
            .shell_command(&command, Some(&mut out), None)
            .map_err(|e| Error::Android(format!("shell `{command}` failed: {e}")))?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Capture a screenshot and write it to `path` as PNG.
    pub fn screenshot_to(&mut self, path: &str) -> Result<()> {
        self.inner
            .framebuffer(&path)
            .map_err(|e| Error::Android(format!("screenshot to `{path}` failed: {e}")))
    }

    /// Capture a screenshot as PNG-encoded bytes.
    pub fn screenshot_bytes(&mut self) -> Result<Vec<u8>> {
        self.inner
            .framebuffer_bytes()
            .map_err(|e| Error::Android(format!("screenshot failed: {e}")))
    }

    /// Dump the current UI accessibility tree as XML.
    pub fn ui_dump(&mut self) -> Result<String> {
        // uiautomator writes to a file on the device; dump then read it back.
        self.shell("uiautomator dump /sdcard/xpressclaw_ui.xml")?;
        self.shell("cat /sdcard/xpressclaw_ui.xml")
    }

    /// Find the first UI element whose `text` or `content-desc` equals `label`.
    pub fn find_element(&mut self, label: &str) -> Result<Option<UiElement>> {
        let xml = self.ui_dump()?;
        Ok(find_node_bounds(&xml, label))
    }

    /// Tap absolute device coordinates.
    pub fn tap(&mut self, x: i32, y: i32) -> Result<()> {
        self.shell(&format!("input tap {x} {y}"))?;
        Ok(())
    }

    /// Resolve `label` to an element and tap its center. Returns the tapped point.
    pub fn tap_text(&mut self, label: &str) -> Result<(i32, i32)> {
        let element = self
            .find_element(label)?
            .ok_or_else(|| Error::Android(format!("no UI element matching '{label}'")))?;
        let (x, y) = element.center();
        self.tap(x, y)?;
        Ok((x, y))
    }
}

/// Find the bounds of the first `<node>` whose `text` or `content-desc`
/// attribute exactly equals `label`. Parses the uiautomator XML directly to
/// avoid a regex dependency.
fn find_node_bounds(xml: &str, label: &str) -> Option<UiElement> {
    let text_attr = format!("text=\"{label}\"");
    let desc_attr = format!("content-desc=\"{label}\"");
    for chunk in xml.split("<node").skip(1) {
        let tag = match chunk.find('>') {
            Some(end) => &chunk[..end],
            None => chunk,
        };
        if tag.contains(&text_attr) || tag.contains(&desc_attr) {
            if let Some(bounds) = parse_bounds(tag) {
                return Some(bounds);
            }
        }
    }
    None
}

/// Parse a `bounds="[x1,y1][x2,y2]"` attribute out of a single node tag.
fn parse_bounds(tag: &str) -> Option<UiElement> {
    let start = tag.find("bounds=\"[")? + "bounds=\"[".len();
    let rest = &tag[start..];
    let end = rest.find("]\"")?;
    let inner = &rest[..end]; // e.g. `561,1406][766,1692`
    let (p1, p2) = inner.split_once("][")?;
    let (x1, y1) = p1.split_once(',')?;
    let (x2, y2) = p2.split_once(',')?;
    Some(UiElement {
        left: x1.trim().parse().ok()?,
        top: y1.trim().parse().ok()?,
        right: x2.trim().parse().ok()?,
        bottom: y2.trim().parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real uiautomator output: the emulator home row we tapped during
    // the spike. Photos' real bounds were [561,1406][766,1692] → center 663,1549.
    const SAMPLE: &str = r#"<hierarchy><node index="0" text="" class="android.widget.FrameLayout" bounds="[0,0][1080,2400]"><node index="1" text="Gmail" content-desc="Gmail" bounds="[100,1406][305,1692]"/><node index="2" text="" content-desc="Photos" bounds="[561,1406][766,1692]"/></node></hierarchy>"#;

    #[test]
    fn finds_element_by_content_desc() {
        let el = find_node_bounds(SAMPLE, "Photos").expect("Photos node");
        assert_eq!(
            el,
            UiElement {
                left: 561,
                top: 1406,
                right: 766,
                bottom: 1692
            }
        );
        // Matches what we measured live on the emulator.
        assert_eq!(el.center(), (663, 1549));
    }

    #[test]
    fn finds_element_by_text() {
        let el = find_node_bounds(SAMPLE, "Gmail").expect("Gmail node");
        assert_eq!(el.center(), (202, 1549));
    }

    #[test]
    fn missing_element_is_none() {
        assert!(find_node_bounds(SAMPLE, "Nonexistent").is_none());
    }

    #[test]
    fn handles_malformed_bounds_gracefully() {
        let xml = r#"<node text="Broken" bounds="[12,][34,56]"/>"#;
        assert!(find_node_bounds(xml, "Broken").is_none());
    }
}
