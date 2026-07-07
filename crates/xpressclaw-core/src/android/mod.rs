//! Android device control over the adb wire protocol (`adb_client`, pure Rust,
//! no `adb` binary). Provider-agnostic: drives whatever `adbd` is reachable —
//! managed emulator or BYO device. Element targeting uses the `uiautomator`
//! accessibility tree, not vision-derived coordinates. See ADR-024.

pub mod emulator;
pub mod sdk;

use std::net::SocketAddr;

use adb_client::{server::ADBServer, tcp::ADBTcpDevice, ADBDeviceExt};
use serde::Serialize;

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

/// One screen-map entry: an interactable/labeled element with its real
/// device-pixel coordinates — the agent's primary perception of the screen.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScreenElement {
    pub text: String,
    pub content_desc: String,
    pub class: String,
    pub clickable: bool,
    /// [left, top, right, bottom] in device pixels.
    pub bounds: [i32; 4],
    /// [x, y] center — the tap target.
    pub center: [i32; 2],
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

    /// Screenshot downscaled to `max_dim` on the longest side, JPEG-encoded at
    /// `quality` — a full frame overflows the agent tool-output limit.
    pub fn screenshot_scaled(&mut self, max_dim: u32, quality: u8) -> Result<Vec<u8>> {
        let png = self.screenshot_bytes()?;
        let img = image::load_from_memory(&png)
            .map_err(|e| Error::Android(format!("decode screenshot: {e}")))?;
        // Only shrink — `resize` fits bounds in both directions and would
        // upscale a frame already smaller than max_dim.
        let img = if img.width().max(img.height()) > max_dim {
            img.resize(max_dim, max_dim, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let rgb = img.to_rgb8();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
            .map_err(|e| Error::Android(format!("encode jpeg: {e}")))?;
        Ok(out)
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

    /// The screen map: all interactable/labeled elements with real coordinates.
    pub fn screen_elements(&mut self) -> Result<Vec<ScreenElement>> {
        let xml = self.ui_dump()?;
        Ok(parse_screen_elements(&xml))
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

    /// Swipe from (x1,y1) to (x2,y2) over `ms` milliseconds.
    pub fn swipe(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, ms: u32) -> Result<()> {
        self.shell(&format!("input swipe {x1} {y1} {x2} {y2} {ms}"))?;
        Ok(())
    }

    /// Type text. Spaces are sent as `%s` (the adb `input text` convention); the
    /// whole argument is single-quoted so quotes, `;`, `$`, backticks, etc. in
    /// the text are treated as literal input, not device-shell metacharacters.
    pub fn input_text(&mut self, text: &str) -> Result<()> {
        let escaped = text.replace(' ', "%s");
        self.shell(&format!("input text {}", sh_quote(&escaped)))?;
        Ok(())
    }

    /// Send a key event — a keycode name (e.g. `KEYCODE_BACK`) or numeric code.
    pub fn key_event(&mut self, key: &str) -> Result<()> {
        self.shell(&format!("input keyevent {key}"))?;
        Ok(())
    }

    /// Long-press at (x, y) for `ms` milliseconds (a zero-distance swipe).
    pub fn long_press(&mut self, x: i32, y: i32, ms: u32) -> Result<()> {
        self.shell(&format!("input swipe {x} {y} {x} {y} {ms}"))?;
        Ok(())
    }

    /// Launch an app by package name via its launcher intent — far more reliable
    /// than hunting for and tapping an icon.
    pub fn open_app(&mut self, package: &str) -> Result<()> {
        // Package names are [A-Za-z0-9._]; anything else could inject device-shell
        // commands or spoof the launch-failure check below.
        if package.is_empty()
            || !package
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'))
        {
            return Err(Error::Android(format!(
                "invalid package name {package:?} (expected characters in [A-Za-z0-9._])"
            )));
        }
        let out = self.shell(&format!(
            "monkey -p {package} -c android.intent.category.LAUNCHER 1"
        ))?;
        if out.contains("No activities found") || out.contains("aborted") {
            return Err(Error::Android(format!(
                "could not launch '{package}' (no launchable activity)"
            )));
        }
        Ok(())
    }

    /// Device screen resolution in pixels, via `wm size` — tap coordinates are
    /// device pixels, not (downscaled) image pixels.
    pub fn screen_size(&mut self) -> Result<(i32, i32)> {
        let out = self.shell("wm size")?;
        parse_wm_size(&out)
            .ok_or_else(|| Error::Android(format!("could not parse `wm size`: {out:?}")))
    }
}

/// POSIX single-quote for the device shell: wrap in `'…'`, rewrite each
/// embedded `'` as `'\''` so no metacharacter can escape.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Parse `wm size` output, e.g. `Physical size: 1080x2400`. Prefers an
/// `Override size:` line when present (that's the effective/captured resolution).
fn parse_wm_size(out: &str) -> Option<(i32, i32)> {
    let line = out
        .lines()
        .find(|l| l.contains("Override size"))
        .or_else(|| out.lines().find(|l| l.contains("Physical size")))?;
    let dims = line.split(':').nth(1)?.trim();
    let (w, h) = dims.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Find the bounds of the first `<node>` whose `text` or `content-desc`
/// attribute exactly equals `label`. Parses the uiautomator XML directly to
/// avoid a regex dependency.
fn find_node_bounds(xml: &str, label: &str) -> Option<UiElement> {
    for chunk in xml.split("<node").skip(1) {
        let tag = match chunk.find('>') {
            Some(end) => &chunk[..end],
            None => chunk,
        };
        // Compare decoded values: "Network & internet" is stored as "&amp;".
        let matches = attr(tag, "text").as_deref() == Some(label)
            || attr(tag, "content-desc").as_deref() == Some(label);
        if matches {
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

/// Read a `name="value"` attribute from a node tag, entity-decoded. The leading
/// space stops `clickable` matching inside `long-clickable`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(xml_decode(&rest[..end]))
}

/// Decode the five XML entities uiautomator emits. `&amp;` is replaced last so
/// an encoded entity like `&amp;lt;` decodes to the literal `&lt;`, not `<`.
fn xml_decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Parse the uiautomator XML into the compact screen map: every node that is
/// clickable or carries text/content-desc, with non-zero bounds.
fn parse_screen_elements(xml: &str) -> Vec<ScreenElement> {
    let mut out = Vec::new();
    for chunk in xml.split("<node").skip(1) {
        let tag = match chunk.find('>') {
            Some(end) => &chunk[..end],
            None => chunk,
        };
        let text = attr(tag, "text").unwrap_or_default();
        let content_desc = attr(tag, "content-desc").unwrap_or_default();
        let clickable = attr(tag, "clickable").as_deref() == Some("true");
        // Drop nodes with nothing useful for the agent to act on.
        if !clickable && text.is_empty() && content_desc.is_empty() {
            continue;
        }
        let Some(b) = parse_bounds(tag) else { continue };
        if b.left == b.right || b.top == b.bottom {
            continue; // zero-area, not tappable
        }
        let (cx, cy) = b.center();
        out.push(ScreenElement {
            text,
            content_desc,
            class: attr(tag, "class").unwrap_or_default(),
            clickable,
            bounds: [b.left, b.top, b.right, b.bottom],
            center: [cx, cy],
        });
    }
    out
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

    #[test]
    fn screen_map_keeps_labeled_and_drops_noise() {
        // Root FrameLayout has no text/desc and isn't clickable → dropped.
        // Gmail + Photos are labeled → kept, with correct centers.
        let els = parse_screen_elements(SAMPLE);
        assert_eq!(els.len(), 2);
        let photos = els.iter().find(|e| e.content_desc == "Photos").unwrap();
        assert_eq!(photos.center, [663, 1549]);
        assert_eq!(photos.bounds, [561, 1406, 766, 1692]);
        let gmail = els.iter().find(|e| e.text == "Gmail").unwrap();
        assert_eq!(gmail.center, [202, 1549]);
    }

    #[test]
    fn attr_does_not_confuse_clickable_with_long_clickable() {
        let tag = r#" index="0" clickable="false" long-clickable="true" "#;
        assert_eq!(attr(tag, "clickable").as_deref(), Some("false"));
    }

    #[test]
    fn sh_quote_neutralizes_metacharacters() {
        assert_eq!(sh_quote("hello"), "'hello'");
        // An embedded single quote is closed, escaped, and reopened.
        assert_eq!(sh_quote("what's"), r"'what'\''s'");
        // Shell metacharacters stay inside the quotes → literal, not executable.
        assert_eq!(sh_quote("a; rm -rf /"), "'a; rm -rf /'");
        assert_eq!(sh_quote("$(reboot)"), "'$(reboot)'");
    }

    #[test]
    fn parses_wm_size() {
        assert_eq!(parse_wm_size("Physical size: 1080x2400"), Some((1080, 2400)));
        // Override wins when present (the effective/captured resolution).
        assert_eq!(
            parse_wm_size("Physical size: 1080x2400\nOverride size: 720x1280"),
            Some((720, 1280))
        );
        assert_eq!(parse_wm_size("garbage"), None);
    }

    #[test]
    fn matches_and_decodes_xml_entities() {
        // uiautomator stores "Network & internet" as "Network &amp; internet".
        let xml = r#"<node text="Network &amp; internet" bounds="[0,100][400,200]"/>"#;
        // The caller types the real label with a literal ampersand.
        let el = find_node_bounds(xml, "Network & internet").expect("decoded match");
        assert_eq!(el.center(), (200, 150));
        // And the screen map emits the decoded text, not the raw entity.
        let els = parse_screen_elements(xml);
        assert_eq!(els[0].text, "Network & internet");
    }
}
