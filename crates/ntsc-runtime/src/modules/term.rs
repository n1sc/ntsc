//! NTSC standard library: `term` module.
//!
//! Terminal ownership and key decoding live in the runtime so applications
//! can implement their screens in Neutron without a crate-specific terminal
//! adaptor. Rendering remains ordinary Neutron output; this module only
//! supplies the platform-sensitive terminal operations.

use std::io::{IsTerminal, Write};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

use crate::registry;

#[derive(Clone, Default)]
struct LastKey {
    code: String,
    character: String,
    ctrl: bool,
}

static ACTIVE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static LAST_KEY: LazyLock<Mutex<LastKey>> = LazyLock::new(|| Mutex::new(LastKey::default()));

fn fail(fn_name: &str, error: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("term.{fn_name}: {error}"))
}

/// `term.is_tty()` — whether stdout is connected to an interactive terminal.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_is_tty() -> i8 {
    i8::from(std::io::stdout().is_terminal())
}

/// `term.open()` — enable raw input and the alternate screen.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_open() -> i64 {
    let mut active = ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *active {
        return 1;
    }
    if let Err(error) = terminal::enable_raw_mode() {
        return fail("open", error);
    }
    let mut stdout = std::io::stdout();
    if let Err(error) = execute!(
        stdout,
        EnterAlternateScreen,
        Hide,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )
    .and_then(|_| stdout.flush())
    {
        let _ = terminal::disable_raw_mode();
        return fail("open", error);
    }
    *active = true;
    1
}

/// `term.restore(handle)` — restore the normal screen and cooked input.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_restore(handle: i64) -> i8 {
    if handle == 0 {
        return 1;
    }
    let mut active = ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !*active {
        return 1;
    }
    let mut ok = terminal::disable_raw_mode().is_ok();
    let mut stdout = std::io::stdout();
    if execute!(
        stdout,
        Show,
        LeaveAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )
    .and_then(|_| stdout.flush())
    .is_err()
    {
        ok = false;
    }
    *active = false;
    i8::from(ok)
}

/// `term.poll_key(handle, timeout_ms)` — wait for a key press.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_poll_key(handle: i64, timeout_ms: f64) -> i8 {
    if handle == 0
        || !*ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        return 0;
    }
    let timeout = Duration::from_millis(timeout_ms.max(0.0) as u64);
    let ready = match event::poll(timeout) {
        Ok(ready) => ready,
        Err(_) => return 0,
    };
    if !ready {
        return 0;
    }
    let Ok(Event::Key(key)) = event::read() else {
        return 0;
    };
    if key.kind != KeyEventKind::Press {
        return 0;
    }
    *LAST_KEY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = decode_key(key);
    1
}

/// `term.key_code()` — the stable code for the last key returned by poll.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_key_code() -> i64 {
    registry::put_string(
        LAST_KEY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .code
            .clone(),
    )
}

/// `term.key_character()` — the character for the last character key.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_key_character() -> i64 {
    registry::put_string(
        LAST_KEY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .character
            .clone(),
    )
}

/// `term.key_ctrl()` — whether the last key had the control modifier.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_term_key_ctrl() -> i8 {
    i8::from(
        LAST_KEY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ctrl,
    )
}

fn decode_key(key: KeyEvent) -> LastKey {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let code = match key.code {
        KeyCode::Char('c') if ctrl => "ctrl_c",
        KeyCode::Char(' ') => "space",
        KeyCode::Char(_) => "character",
        KeyCode::Enter => "enter",
        KeyCode::Esc => "escape",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Backspace => "backspace",
        KeyCode::Tab => "tab",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::Delete => "delete",
        KeyCode::PageUp => "page_up",
        KeyCode::PageDown => "page_down",
        _ => "other",
    };
    let character = match key.code {
        KeyCode::Char(character) => character.to_string(),
        _ => String::new(),
    };
    LastKey {
        code: code.to_string(),
        character,
        ctrl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_control_and_character_keys() {
        let key = decode_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(key.code, "character");
        assert_eq!(key.character, "b");
        assert!(key.ctrl);

        let key = decode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(key.code, "enter");
        assert!(key.character.is_empty());
        assert!(!key.ctrl);
    }
}
