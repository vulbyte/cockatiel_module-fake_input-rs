//! The controller bridge: a virtual gamepad driven by chat inputs.
//!
//! On Linux this creates a uinput virtual gamepad (`/dev/uinput`); on Windows
//! it drives a ViGEmBus virtual Xbox 360 controller. On macOS the backend is a
//! bridge to a signed helper app (CoreHID `HIDVirtualDevice`) that does not
//! exist yet — until then, gamepad inputs report "unsupported on macOS".
//!
//! The interface is identical on every OS: `press_button` / `release_button`
//! / `set_stick` / `release_stick`, driven by whitelisted `GamepadAction`s.

/// One whitelisted gamepad action a viewer can trigger via `!input`.
#[derive(Debug, Clone, PartialEq)]
pub enum GamepadAction {
    /// Press a named button (Xbox-style: A/B/X/Y, LB/RB, LT/RT, Start/Back).
    Button { button: String },
    /// Move a stick to a position in [-1, 1]; `-1/1` are the extremes.
    Stick {
        stick: String,
        x: f64,
        y: f64,
    },
}

/// Map a config `stick` string to its canonical name ("left" | "right").
pub fn normalize_stick(name: &str) -> Option<String> {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "left" | "right" => Some(n),
        _ => None,
    }
}

/// The virtual gamepad backend for the current platform.
///
/// Holds the live device/controller handle so buttons and sticks can be
/// pressed, released and moved repeatedly without re-creating it.
#[derive(Debug)]
pub struct Gamepad {
    #[cfg(target_os = "linux")]
    inner: Option<evdev::uinput::VirtualDevice>,
    #[cfg(target_os = "windows")]
    inner: Option<vigem_rust::X360Target>,
    #[cfg(target_os = "macos")]
    #[allow(dead_code)]
    inner: Option<MacUnsupported>,
    /// Button/stick state so a release only happens after a press (a stick is
    /// never double-centered, a button never double-released).
    buttons_held: std::collections::HashSet<String>,
    sticks_held: std::collections::HashMap<String, (f64, f64)>,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacUnsupported;

/// The single Xbox-style button set shared by every backend, so the config's
/// button names are identical across platforms. (Linux + macOS map onto these;
/// Windows passes them straight to the X360 report.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XButton {
    A,
    B,
    X,
    Y,
    LB,
    RB,
    LT,
    RT,
    Start,
    Back,
    Guide,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl XButton {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "a" => Some(Self::A),
            "b" => Some(Self::B),
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            "lb" => Some(Self::LB),
            "rb" => Some(Self::RB),
            "lt" => Some(Self::LT),
            "rt" => Some(Self::RT),
            "start" => Some(Self::Start),
            "back" => Some(Self::Back),
            "guide" => Some(Self::Guide),
            "dpad-up" => Some(Self::DpadUp),
            "dpad-down" => Some(Self::DpadDown),
            "dpad-left" => Some(Self::DpadLeft),
            "dpad-right" => Some(Self::DpadRight),
            _ => None,
        }
    }
}

impl Gamepad {
    /// Create the virtual gamepad for the current OS. Returns an error string
    /// describing why it can't be created (e.g. missing `/dev/uinput`, missing
    /// ViGEmBus, or macOS unsupported).
    pub fn create() -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            Self::create_linux()
        }
        #[cfg(target_os = "windows")]
        {
            Self::create_windows()
        }
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                inner: Some(MacUnsupported),
                buttons_held: std::collections::HashSet::new(),
                sticks_held: std::collections::HashMap::new(),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Err("gamepad not supported on this platform".to_string())
        }
    }

    /// Whether this platform can actually inject controller input right now.
    pub fn supported() -> bool {
        #[cfg(target_os = "linux")]
        {
            true
        }
        #[cfg(target_os = "windows")]
        {
            true
        }
        #[cfg(target_os = "macos")]
        {
            false // until the signed helper ships
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            false
        }
    }

    /// The human-facing reason a platform doesn't support gamepad input yet.
    pub fn unsupported_reason() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            "controller bridge is not yet supported on macOS (a signed helper is required); keyboard/mouse inputs still work"
        }
        #[cfg(not(target_os = "macos"))]
        {
            "gamepad not supported on this platform"
        }
    }

    /// Press (and hold) a button. Idempotent.
    pub fn press(&mut self, action: &GamepadAction) -> Result<(), String> {
        let name = match action {
            GamepadAction::Button { button } => button,
            _ => return Ok(()),
        };
        let _btn = XButton::from_name(name).ok_or_else(|| format!("unknown button '{name}'"))?;
        if !self.buttons_held.insert(name.clone()) {
            return Ok(()); // already held
        }
        #[cfg(target_os = "linux")]
        {
            self.linux_press(btn)
        }
        #[cfg(target_os = "windows")]
        {
            self.windows_press(btn)
        }
        #[cfg(target_os = "macos")]
        {
            Err(Self::unsupported_reason().to_string())
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Err(Self::unsupported_reason().to_string())
        }
    }

    /// Release a held button. Idempotent.
    pub fn release(&mut self, action: &GamepadAction) -> Result<(), String> {
        let name = match action {
            GamepadAction::Button { button } => button,
            _ => return Ok(()),
        };
        let _btn = XButton::from_name(name).ok_or_else(|| format!("unknown button '{name}'"))?;
        if !self.buttons_held.remove(name) {
            return Ok(()); // not held
        }
        #[cfg(target_os = "linux")]
        {
            self.linux_release(btn)
        }
        #[cfg(target_os = "windows")]
        {
            self.windows_release(btn)
        }
        #[cfg(target_os = "macos")]
        {
            Err(Self::unsupported_reason().to_string())
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Err(Self::unsupported_reason().to_string())
        }
    }

    /// Set a stick to a position in [-1, 1]. Idempotent (repeated sets are
    /// fine; a release centers it).
    pub fn set_stick(&mut self, action: &GamepadAction) -> Result<(), String> {
        let (name, x, y) = match action {
            GamepadAction::Stick { stick, x, y } => (stick.clone(), *x, *y),
            _ => return Ok(()),
        };
        if normalize_stick(&name).is_none() {
            return Err(format!("unknown stick '{name}'"));
        }
        self.sticks_held.insert(name.clone(), (x, y));
        #[cfg(target_os = "linux")]
        {
            self.linux_set_stick(&name, x, y)
        }
        #[cfg(target_os = "windows")]
        {
            self.windows_set_stick(&name, x, y)
        }
        #[cfg(target_os = "macos")]
        {
            Err(Self::unsupported_reason().to_string())
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Err(Self::unsupported_reason().to_string())
        }
    }

    /// Release (center) a held stick.
    pub fn release_stick(&mut self, action: &GamepadAction) -> Result<(), String> {
        let (name, _x, _y) = match action {
            GamepadAction::Stick { stick, x, y } => (stick.clone(), *x, *y),
            _ => return Ok(()),
        };
        if self.sticks_held.remove(&name).is_none() {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            self.linux_release_stick(&name)
        }
        #[cfg(target_os = "windows")]
        {
            self.windows_release_stick(&name)
        }
        #[cfg(target_os = "macos")]
        {
            Err(Self::unsupported_reason().to_string())
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Err(Self::unsupported_reason().to_string())
        }
    }

    // ── Linux backend (uinput) ─────────────────────────────────────────

    #[cfg(target_os = "linux")]
    fn create_linux() -> Result<Self, String> {
        use evdev::uinput::VirtualDeviceBuilder;
        use evdev::{AbsInfo, AbsoluteAxisCode, AttributeSet, KeyCode, UinputAbsSetup};

        let mut keys = AttributeSet::<KeyCode>::new();
        // Gamepad face + shoulder + system buttons (Xbox-style mapping).
        for code in [
            KeyCode::BTN_SOUTH, // A
            KeyCode::BTN_EAST,  // B
            KeyCode::BTN_NORTH, // X
            KeyCode::BTN_WEST,  // Y
            KeyCode::BTN_TL,    // LB
            KeyCode::BTN_TR,    // RB
            KeyCode::BTN_TL2,   // LT
            KeyCode::BTN_TR2,   // RT
            KeyCode::BTN_SELECT,
            KeyCode::BTN_START,
            KeyCode::BTN_MODE,
        ] {
            keys.insert(code);
        }
        // D-pad via hat switch axes.
        let mut builder = VirtualDeviceBuilder::builder()
            .map_err(|e| format!("uinput builder: {e}"))?
            .with_keys(&keys)
            .map_err(|e| format!("uinput keys: {e}"))?;
        // Left + right sticks (ABS_X/Y = left, ABS_RX/RY = right), range -32768..32767.
        let axis = |code: AbsoluteAxisCode| -> evdev::UinputAbsSetup {
            UinputAbsSetup::new(code, AbsInfo::new(0, -32768, 32767, 0, 0, 0))
        };
        for code in [
            AbsoluteAxisCode::ABS_X,
            AbsoluteAxisCode::ABS_Y,
            AbsoluteAxisCode::ABS_RX,
            AbsoluteAxisCode::ABS_RY,
            AbsoluteAxisCode::ABS_HAT0X,
            AbsoluteAxisCode::ABS_HAT0Y,
        ] {
            builder = builder
                .with_absolute_axis(&axis(code))
                .map_err(|e| format!("uinput abs axis: {e}"))?;
        }
        let dev = builder
            .name("cockatiel-fake-input")
            .build()
            .map_err(|e| format!("uinput build: {e}"))?;
        Ok(Self {
            inner: Some(dev),
            buttons_held: std::collections::HashSet::new(),
            sticks_held: std::collections::HashMap::new(),
        })
    }

    #[cfg(target_os = "linux")]
    fn linux_keycode(btn: XButton) -> evdev::KeyCode {
        use evdev::KeyCode;
        match btn {
            XButton::A => KeyCode::BTN_SOUTH,
            XButton::B => KeyCode::BTN_EAST,
            XButton::X => KeyCode::BTN_NORTH,
            XButton::Y => KeyCode::BTN_WEST,
            XButton::LB => KeyCode::BTN_TL,
            XButton::RB => KeyCode::BTN_TR,
            XButton::LT => KeyCode::BTN_TL2,
            XButton::RT => KeyCode::BTN_TR2,
            XButton::Start => KeyCode::BTN_START,
            XButton::Back => KeyCode::BTN_SELECT,
            XButton::Guide => KeyCode::BTN_MODE,
            XButton::DpadUp | XButton::DpadDown | XButton::DpadLeft | XButton::DpadRight => {
                // D-pad handled via the hat axes; map to a no-op button here.
                KeyCode::BTN_TRIGGER
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_emit(&mut self, events: &[evdev::InputEvent]) -> Result<(), String> {
        if let Some(dev) = self.inner.as_mut() {
            dev.emit(events).map_err(|e| format!("uinput emit: {e}"))?;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn linux_press(&mut self, btn: XButton) -> Result<(), String> {
        use evdev::{KeyEvent, KeyState};
        if matches!(
            btn,
            XButton::DpadUp | XButton::DpadDown | XButton::DpadLeft | XButton::DpadRight
        ) {
            return self.linux_dpad(btn, true);
        }
        let code = Self::linux_keycode(btn);
        self.linux_emit(&[KeyEvent::new(code, KeyState::PRESSED).into()])
    }

    #[cfg(target_os = "linux")]
    fn linux_release(&mut self, btn: XButton) -> Result<(), String> {
        use evdev::{KeyEvent, KeyState};
        if matches!(
            btn,
            XButton::DpadUp | XButton::DpadDown | XButton::DpadLeft | XButton::DpadRight
        ) {
            return self.linux_dpad(btn, false);
        }
        let code = Self::linux_keycode(btn);
        self.linux_emit(&[KeyEvent::new(code, KeyState::RELEASED).into()])
    }

    #[cfg(target_os = "linux")]
    fn linux_dpad(&mut self, btn: XButton, pressed: bool) -> Result<(), String> {
        use evdev::{AbsoluteAxisCode, AbsoluteAxisEvent, KeyEvent, KeyState};
        let val = if pressed { 1 } else { 0 };
        let events: Vec<evdev::InputEvent> = match btn {
            XButton::DpadUp => vec![AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0Y, -val).into()],
            XButton::DpadDown => vec![AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0Y, val).into()],
            XButton::DpadLeft => vec![AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0X, -val).into()],
            XButton::DpadRight => vec![AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0X, val).into()],
            _ => vec![],
        };
        self.linux_emit(&events)
    }

    #[cfg(target_os = "linux")]
    fn linux_set_stick(&mut self, name: &str, x: f64, y: f64) -> Result<(), String> {
        use evdev::{AbsoluteAxisCode, AbsoluteAxisEvent};
        let (x_axis, y_axis) = self.linux_stick_axes(name);
        let scale = |v: f64| -> i32 { (v.clamp(-1.0, 1.0) * 32767.0) as i32 };
        let events = vec![
            AbsoluteAxisEvent::new(x_axis, scale(x)).into(),
            AbsoluteAxisEvent::new(y_axis, scale(y)).into(),
        ];
        self.linux_emit(&events)
    }

    #[cfg(target_os = "linux")]
    fn linux_release_stick(&mut self, name: &str) -> Result<(), String> {
        self.linux_set_stick(name, 0.0, 0.0)
    }

    #[cfg(target_os = "linux")]
    fn linux_stick_axes(&self, name: &str) -> (evdev::AbsoluteAxisCode, evdev::AbsoluteAxisCode) {
        use evdev::AbsoluteAxisCode;
        match name {
            "left" => (AbsoluteAxisCode::ABS_X, AbsoluteAxisCode::ABS_Y),
            _ => (AbsoluteAxisCode::ABS_RX, AbsoluteAxisCode::ABS_RY),
        }
    }

    // ── Windows backend (ViGEmBus) ──────────────────────────────────────

    #[cfg(target_os = "windows")]
    fn create_windows() -> Result<Self, String> {
        let client = vigem_rust::Client::connect().map_err(|e| format!("ViGEmBus connect: {e}"))?;
        let target = client
            .new_x360_target()
            .plug()
            .map_err(|e| format!("ViGEm plug: {e}"))?;
        let target = target.wait_for_ready().map_err(|e| format!("ViGEm ready: {e}"))?;
        Ok(Self {
            inner: Some(target),
            buttons_held: std::collections::HashSet::new(),
            sticks_held: std::collections::HashMap::new(),
        })
    }

    #[cfg(target_os = "windows")]
    fn windows_report(
        buttons: &std::collections::HashSet<String>,
        sticks: &std::collections::HashMap<String, (f64, f64)>,
    ) -> vigem_rust::X360Report {
        use vigem_rust::{X360Button, X360Report};
        let mut report = X360Report::default();
        for held in buttons {
            let b = match XButton::from_name(held) {
                Some(XButton::A) => X360Button::A,
                Some(XButton::B) => X360Button::B,
                Some(XButton::X) => X360Button::X,
                Some(XButton::Y) => X360Button::Y,
                Some(XButton::LB) => X360Button::LB,
                Some(XButton::RB) => X360Button::RB,
                Some(XButton::Start) => X360Button::Start,
                Some(XButton::Back) => X360Button::Back,
                Some(XButton::Guide) => X360Button::Guide,
                Some(XButton::DpadUp) => X360Button::DpadUp,
                Some(XButton::DpadDown) => X360Button::DpadDown,
                Some(XButton::DpadLeft) => X360Button::DpadLeft,
                Some(XButton::DpadRight) => X360Button::DpadRight,
                _ => continue,
            };
            report.buttons.insert(b);
        }
        // Sticks: clamp [-1,1] to the XInput i16 range.
        let scale = |v: f64| -> i16 { (v.clamp(-1.0, 1.0) * 32767.0) as i16 };
        if let Some((x, y)) = sticks.get("left") {
            report.thumb_lx = scale(*x);
            report.thumb_ly = scale(*y);
        }
        if let Some((x, y)) = sticks.get("right") {
            report.thumb_rx = scale(*x);
            report.thumb_ry = scale(*y);
        }
        report
    }

    #[cfg(target_os = "windows")]
    fn windows_update(&mut self) -> Result<(), String> {
        let report = Self::windows_report(&self.buttons_held, &self.sticks_held);
        if let Some(target) = self.inner.as_mut() {
            target.update(&report).map_err(|e| format!("ViGEm update: {e}"))?;
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn windows_press(&mut self, _btn: XButton) -> Result<(), String> {
        self.windows_update()
    }
    #[cfg(target_os = "windows")]
    fn windows_release(&mut self, _btn: XButton) -> Result<(), String> {
        self.windows_update()
    }
    #[cfg(target_os = "windows")]
    fn windows_set_stick(&mut self, _n: &str, _x: f64, _y: f64) -> Result<(), String> {
        self.windows_update()
    }
    #[cfg(target_os = "windows")]
    fn windows_release_stick(&mut self, _n: &str) -> Result<(), String> {
        self.windows_update()
    }
}

/// A button click = press + release, used for `!input <name>` taps.
pub fn click(g: &mut Gamepad, action: &GamepadAction) -> Result<(), String> {
    g.press(action)?;
    g.release(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_names_are_left_or_right() {
        assert_eq!(normalize_stick("Left").as_deref(), Some("left"));
        assert_eq!(normalize_stick("right").as_deref(), Some("right"));
        assert_eq!(normalize_stick("diagonal"), None);
    }

    #[test]
    fn x_button_parses_known_names() {
        assert_eq!(XButton::from_name("A"), Some(XButton::A));
        assert_eq!(XButton::from_name("rt"), Some(XButton::RT));
        assert_eq!(XButton::from_name("Start"), Some(XButton::Start));
        assert_eq!(XButton::from_name("x"), Some(XButton::X));
        assert_eq!(XButton::from_name("??"), None);
    }

    #[test]
    fn gamepad_platform_support_does_not_crash() {
        // Just confirms the enum + constructor compile and run on this platform.
        let g = Gamepad::create();
        let _ = g;
    }
}