//! `tuishou top`: one terminal hosting `reiki top` and `lu top`.
//!
//! Each child owns a pseudo-terminal and its own vt100 screen. Only the active
//! screen is painted, but both children keep running, so their selection,
//! scroll and filter state survives a swap. The host owns raw mode and the
//! alternate screen; every exit path restores them and signals both children.

use crate::config::LoadedConfig;
use crate::error::{Error, Result};
use crate::state;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::LeaveAlternateScreen;
use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::DefaultTerminal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use tui_term::widget::PseudoTerminal;
use vt100::Parser;

#[cfg(unix)]
use nix::sys::signal::{killpg, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(750);
const RECENT_LIMIT: usize = 16 * 1024;
const HELP_TEXT: &str =
    "Alt+1/F1 reiki | Alt+2/F2 lu | Ctrl+Tab cycle | Ctrl+b: r restart, q quit, ? help";

const MOUSE_ENABLE: [&str; 6] = ["?1000h", "?1002h", "?1003h", "?1004h", "?1006h", "?1015h"];
const MOUSE_DISABLE: [&str; 6] = ["?1000l", "?1002l", "?1003l", "?1004l", "?1006l", "?1015l"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Reiki,
    Lu,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Self::Reiki => 0,
            Self::Lu => 1,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Reiki => "reiki",
            Self::Lu => "lu",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Reiki => Self::Lu,
            Self::Lu => Self::Reiki,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "reiki" => Some(Self::Reiki),
            "lu" => Some(Self::Lu),
            _ => None,
        }
    }
}

const TABS: [Tab; 2] = [Tab::Reiki, Tab::Lu];

/// Keys are represented independently of crossterm so routing is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Alt(char),
    Ctrl(char),
    F(u8),
    CtrlTab,
    Tab,
    Esc,
    Enter,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCommand {
    Restart,
    Quit,
    Help,
    Cancel,
    ForwardPrefix,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Switch(Tab),
    Cycle,
    StartPrefix,
    Host(HostCommand),
    Send(Key),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Prefix,
    Help,
}

/// The whole key state machine. Normal keys go to the active child; the prefix
/// opens a small host command mode. Help mode swallows one key to close.
pub fn route_key(mode: Mode, key: Key) -> Action {
    match mode {
        Mode::Normal => match key {
            Key::Alt('1') | Key::F(1) => Action::Switch(Tab::Reiki),
            Key::Alt('2') | Key::F(2) => Action::Switch(Tab::Lu),
            Key::CtrlTab => Action::Cycle,
            Key::Ctrl('b') => Action::StartPrefix,
            Key::Esc
            | Key::Tab
            | Key::Enter
            | Key::Backspace
            | Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Delete
            | Key::Insert
            | Key::Char(_)
            | Key::Alt(_)
            | Key::Ctrl(_)
            | Key::F(_) => Action::Send(key),
        },
        Mode::Prefix => match key {
            Key::Char('r') => Action::Host(HostCommand::Restart),
            Key::Char('q') => Action::Host(HostCommand::Quit),
            Key::Char('?') => Action::Host(HostCommand::Help),
            Key::Char('b') | Key::Ctrl('b') => Action::Host(HostCommand::ForwardPrefix),
            Key::Esc => Action::Host(HostCommand::Cancel),
            _ => Action::Host(HostCommand::Unknown),
        },
        Mode::Help => Action::Host(HostCommand::Cancel),
    }
}

fn key_from_event(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let key = match event.code {
        KeyCode::Char(c) if alt => Key::Alt(c),
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::F(n) => Key::F(n),
        KeyCode::Tab if ctrl => Key::CtrlTab,
        KeyCode::Tab => Key::Tab,
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        _ => return None,
    };
    Some(key)
}

fn encode_key(key: Key) -> Vec<u8> {
    match key {
        Key::Char(c) => c.to_string().into_bytes(),
        Key::Alt(c) => {
            let mut bytes = vec![0x1b];
            bytes.extend_from_slice(c.to_string().as_bytes());
            bytes
        }
        Key::Ctrl(c) => vec![c.to_ascii_uppercase() as u8 & 0x1f],
        Key::Tab => b"\t".to_vec(),
        Key::Esc => b"\x1b".to_vec(),
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Up => b"\x1b[A".to_vec(),
        Key::Down => b"\x1b[B".to_vec(),
        Key::Right => b"\x1b[C".to_vec(),
        Key::Left => b"\x1b[D".to_vec(),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::CtrlTab => b"\x1b[1;5I".to_vec(),
        Key::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5..=10 => format!("\x1b[{}~", n + 10).into_bytes(),
            11..=12 => format!("\x1b[{}~", n + 12).into_bytes(),
            _ => Vec::new(),
        },
    }
}

fn encode_mouse(event: MouseEvent) -> Vec<u8> {
    let (button, down) = match event.kind {
        MouseEventKind::Down(b) => (button_bits(b), true),
        MouseEventKind::Up(b) => (button_bits(b), false),
        MouseEventKind::Drag(b) => (button_bits(b) + 32, true),
        MouseEventKind::Moved => (35, true),
        MouseEventKind::ScrollUp => (64, true),
        MouseEventKind::ScrollDown => (65, true),
        MouseEventKind::ScrollLeft => (66, true),
        MouseEventKind::ScrollRight => (67, true),
    };
    let mut code = button;
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let trailer = if down { 'M' } else { 'm' };
    format!(
        "\x1b[<{code};{};{}{trailer}",
        event.column.saturating_add(1),
        event.row.saturating_add(1)
    )
    .into_bytes()
}

fn button_bits(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Track the last mouse-tracking mode requested by the active child. Later
/// sequences win so enable/disable pairs are respected in order.
fn update_mouse_mode(current: bool, bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut mode = current;
    let mut pos = 0;
    loop {
        let enable = MOUSE_ENABLE
            .iter()
            .filter_map(|needle| text[pos..].find(needle).map(|at| pos + at))
            .min();
        let disable = MOUSE_DISABLE
            .iter()
            .filter_map(|needle| text[pos..].find(needle).map(|at| pos + at))
            .min();
        match (enable, disable) {
            (Some(on), Some(off)) if on <= off => {
                mode = true;
                pos = on + 6;
            }
            (Some(_on), Some(off)) => {
                mode = false;
                pos = off + 6;
            }
            (Some(on), None) => {
                mode = true;
                pos = on + 6;
            }
            (None, Some(off)) => {
                mode = false;
                pos = off + 6;
            }
            (None, None) => return mode,
        }
    }
}

/// Split an override into an argv without involving a shell. Quotes group
/// arguments; backslash escapes the next character; nothing is expanded.
pub fn split_command(raw: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some('"') if ch == '\\' => match chars.next() {
                Some(next) => current.push(next),
                None => {
                    return Err(Error::Usage(
                        "unterminated escape in command override".into(),
                    ))
                }
            },
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None if ch == '\\' => match chars.next() {
                Some(next) => {
                    current.push(next);
                    started = true;
                }
                None => {
                    return Err(Error::Usage(
                        "unterminated escape in command override".into(),
                    ))
                }
            },
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(Error::Usage(
            "unterminated quote in command override".into(),
        ));
    }
    if started {
        args.push(current);
    }
    if args.is_empty() {
        return Err(Error::Usage("empty command override".into()));
    }
    Ok(args)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct TopState {
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    children: BTreeMap<String, ChildState>,
}

impl TopState {
    fn active_tab(&self) -> Tab {
        self.active
            .as_deref()
            .and_then(Tab::from_name)
            .unwrap_or(Tab::Reiki)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ChildState {
    #[serde(default)]
    restart_count: u64,
    #[serde(default)]
    last_args: Vec<String>,
}

fn load_state(path: &Path) -> TopState {
    match state::read_private_string(path) {
        Some(text) => serde_json::from_str(&text).unwrap_or_default(),
        None => TopState::default(),
    }
}

/// A child's status line, plus a hint that never contains its argv.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExitInfo {
    status: String,
    hint: String,
}

impl ExitInfo {
    fn startup(error: &str, argv: &[String], name: &str) -> Self {
        let program = argv
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "child".to_string());
        let program = crate::logging::one_line(&crate::http::redact_secrets(&program));
        let text = crate::logging::one_line(error);
        let hint = if text.to_ascii_lowercase().contains("no such file")
            || text.to_ascii_lowercase().contains("not found")
        {
            format!(
                "binary '{program}' was not found; install a tui-enabled build with `tuishou install {name}` or pass --{name}-cmd"
            )
        } else {
            format!("could not start '{program}': {text}")
        };
        Self {
            status: "failed to start".to_string(),
            hint,
        }
    }

    fn exited(status: portable_pty::ExitStatus, recent: &[u8]) -> Self {
        let label = match status.signal() {
            Some(signal) => format!("signal {signal}"),
            None => format!("status {}", status.exit_code()),
        };
        Self {
            status: label,
            hint: exit_hint(recent, status.exit_code()),
        }
    }

    fn message(&self, name: &str) -> String {
        if self.hint.is_empty() {
            format!("{name} exited: {}", self.status)
        } else {
            format!("{name} exited: {}. {}", self.status, self.hint)
        }
    }
}

fn exit_hint(recent: &[u8], code: u32) -> String {
    let text = String::from_utf8_lossy(recent);
    let flat = crate::logging::one_line(&text);
    let flat: String = flat.chars().take(200).collect();
    let lower = flat.to_ascii_lowercase();
    if code == 127 || lower.contains("no such file") || lower.contains("not found") {
        return format!("binary may be missing; install a tui-enabled build with `tuishou install <component>` or pass --<component>-cmd. {flat}");
    }
    if lower.contains("tui")
        || lower.contains("feature")
        || lower.contains("unrecognized subcommand")
        || lower.contains("unknown subcommand")
    {
        return format!("this child may lack the `tui` feature; install a tui-enabled release with `tuishou install <component>` (or rebuild with `--features tui`), then press Ctrl+b r. {flat}");
    }
    flat
}

struct Pane {
    argv: Vec<String>,
    parser: Parser,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    output: mpsc::Receiver<Vec<u8>>,
    _output_keepalive: mpsc::Sender<Vec<u8>>,
    exit: Option<ExitInfo>,
    mouse_enabled: bool,
    recent: Vec<u8>,
    restart_count: u64,
    pgid: Option<i32>,
}

impl Pane {
    fn new(argv: Vec<String>, rows: u16, cols: u16, state: &ChildState) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            argv,
            parser: Parser::new(rows, cols, 0),
            master: None,
            writer: None,
            child: None,
            output: rx,
            _output_keepalive: tx,
            exit: None,
            mouse_enabled: false,
            recent: Vec::new(),
            restart_count: state.restart_count,
            pgid: None,
        }
    }

    fn spawn(
        &mut self,
        argv: Vec<String>,
        rows: u16,
        cols: u16,
        config_path: Option<&Path>,
        name: &str,
    ) {
        self.stop();
        self.argv = argv;
        self.parser = Parser::new(rows, cols, 0);
        self.writer = None;
        self.master = None;
        self.exit = None;
        self.mouse_enabled = false;
        self.recent.clear();
        self.pgid = None;
        if let Err(err) = self.spawn_inner(rows, cols, config_path) {
            self.exit = Some(ExitInfo::startup(&err.to_string(), &self.argv, name));
        }
    }

    fn spawn_inner(&mut self, rows: u16, cols: u16, config_path: Option<&Path>) -> Result<()> {
        let program = crate::process::resolve_program_with_path(
            &self.argv[0],
            OsStr::new(crate::process::SAFE_PATH),
        )?;
        let mut command = CommandBuilder::new(&program);
        command.args(&self.argv[1..]);
        command.env_clear();
        command.env("PATH", crate::process::SAFE_PATH);
        let term = std::env::var_os("TERM").unwrap_or_else(|| "xterm-256color".into());
        command.env("TERM", term);
        for key in crate::process::ENV_PASSTHROUGH {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if let Some(path) = config_path {
            command.env("TUISHOU_CONFIG", path);
        }
        let pty_system = NativePtySystem::default();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| Error::msg(format!("cannot allocate pty: {e}")))?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| Error::msg(format!("cannot spawn child: {e}")))?;
        drop(pair.slave);
        let pgid = pair
            .master
            .process_group_leader()
            .or_else(|| child.process_id().and_then(|pid| i32::try_from(pid).ok()));
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::msg(format!("cannot open pty writer: {e}")))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::msg(format!("cannot open pty reader: {e}")))?;
        let tx = self._output_keepalive.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        self.master = Some(pair.master);
        self.writer = Some(writer);
        self.child = Some(child);
        self.pgid = pgid;
        Ok(())
    }

    /// Drain output from both children, including the inactive one, so their
    /// vt100 screens stay in sync while swapped out.
    fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(chunk) = self.output.try_recv() {
            self.parser.process(&chunk);
            self.mouse_enabled = update_mouse_mode(self.mouse_enabled, &chunk);
            if self.recent.len() < RECENT_LIMIT {
                let keep = RECENT_LIMIT - self.recent.len();
                self.recent
                    .extend_from_slice(&chunk[..chunk.len().min(keep)]);
            }
            changed = true;
        }
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(None) => self.child = Some(child),
                Ok(Some(status)) => {
                    // The child is a session leader; signal its whole group so
                    // grandchildren do not survive the host.
                    self.hangup();
                    let info = ExitInfo::exited(status, &self.recent);
                    self.exit = Some(info);
                    changed = true;
                }
                Err(_) => {
                    self.exit = Some(ExitInfo {
                        status: "wait failed".to_string(),
                        hint: String::new(),
                    });
                    changed = true;
                }
            }
        }
        changed
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if let Some(master) = self.master.as_ref() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        self.parser.screen_mut().set_size(rows, cols);
    }

    fn hangup(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGHUP);
        }
    }

    fn force_kill(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
    }

    /// SIGHUP first, SIGKILL after a grace period, then reap. Safe to call
    /// repeatedly (including from Drop after a clean shutdown).
    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        self.hangup();
        #[cfg(not(unix))]
        let _ = child.kill();
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        let mut reaped = false;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    reaped = true;
                    break;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => break,
            }
        }
        if !reaped {
            self.force_kill();
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
        }
        self.pgid = None;
        self.writer = None;
        self.master = None;
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Host {
    terminal: DefaultTerminal,
    panes: [Pane; 2],
    active: Tab,
    mode: Mode,
    message: Option<String>,
    state_path: PathBuf,
    state: TopState,
    rows: u16,
    cols: u16,
    mouse_capture: bool,
    config_path: Option<PathBuf>,
    dirty: bool,
}

impl Host {
    #[allow(clippy::too_many_arguments)]
    fn new(
        terminal: DefaultTerminal,
        loaded: &LoadedConfig,
        state_path: PathBuf,
        saved: TopState,
        active: Tab,
        reiki_cmd: Option<&str>,
        lu_cmd: Option<&str>,
    ) -> Result<Self> {
        let area = terminal
            .size()
            .map_err(|e| Error::msg(format!("terminal size: {e}")))?;
        let cols = area.width.max(1);
        let rows = area.height.saturating_sub(1).max(1);
        let commands = [
            command_for(Tab::Reiki, loaded, reiki_cmd, &saved)?,
            command_for(Tab::Lu, loaded, lu_cmd, &saved)?,
        ];
        let panes = TABS.map(|tab| {
            let index = tab.index();
            let child_state = saved.children.get(tab.label()).cloned().unwrap_or_default();
            Pane::new(commands[index].clone(), rows, cols, &child_state)
        });
        let mut host = Self {
            terminal,
            panes,
            active,
            mode: Mode::Normal,
            message: None,
            state_path,
            state: saved,
            rows,
            cols,
            mouse_capture: false,
            config_path: loaded
                .config_path
                .exists()
                .then(|| loaded.config_path.clone()),
            dirty: true,
        };
        for tab in TABS {
            let pane = &mut host.panes[tab.index()];
            let argv = pane.argv.clone();
            pane.spawn(argv, rows, cols, host.config_path.as_deref(), tab.label());
            if let Some(info) = &pane.exit {
                host.message = Some(info.message(tab.label()));
            }
        }
        if let Err(err) = host.persist() {
            host.message = Some(format!(
                "cannot save top state: {}",
                crate::http::redact_secrets(&err.to_string())
            ));
        }
        Ok(host)
    }

    fn active_pane(&self) -> &Pane {
        &self.panes[self.active.index()]
    }

    fn persist(&self) -> Result<()> {
        let mut snapshot = self.state.clone();
        snapshot.active = Some(self.active.label().to_string());
        for tab in TABS {
            let pane = &self.panes[tab.index()];
            snapshot.children.insert(
                tab.label().to_string(),
                ChildState {
                    restart_count: pane.restart_count,
                    // Executable argv is never persisted: a state file must
                    // not become a source of commands on a later run.
                    last_args: Vec::new(),
                },
            );
        }
        state::save_value(&self.state_path, &snapshot)
    }

    fn select(&mut self, tab: Tab) {
        if self.active != tab {
            self.active = tab;
            if let Err(err) = self.persist() {
                self.message = Some(format!(
                    "cannot save top state: {}",
                    crate::http::redact_secrets(&err.to_string())
                ));
            }
        }
        self.mode = Mode::Normal;
        self.dirty = true;
    }

    fn restart_active(&mut self) {
        let tab = self.active;
        let pane = &mut self.panes[tab.index()];
        let argv = pane.argv.clone();
        pane.spawn(
            argv,
            self.rows,
            self.cols,
            self.config_path.as_deref(),
            tab.label(),
        );
        pane.restart_count += 1;
        self.message = match &pane.exit {
            Some(info) => Some(info.message(tab.label())),
            None => Some(format!("{} restarted", tab.label())),
        };
        self.dirty = true;
        if let Err(err) = self.persist() {
            self.message = Some(format!(
                "cannot save top state: {}",
                crate::http::redact_secrets(&err.to_string())
            ));
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols.max(1);
        self.rows = rows.saturating_sub(1).max(1);
        for pane in &mut self.panes {
            pane.resize(self.rows, self.cols);
        }
        self.dirty = true;
    }

    fn refresh_mouse_capture(&mut self) -> Result<()> {
        let wanted = self.active_pane().mouse_enabled;
        if wanted == self.mouse_capture {
            return Ok(());
        }
        if wanted {
            execute!(io::stdout(), EnableMouseCapture)
                .map_err(|e| Error::msg(format!("mouse capture: {e}")))?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)
                .map_err(|e| Error::msg(format!("mouse capture: {e}")))?;
        }
        self.mouse_capture = wanted;
        Ok(())
    }

    fn pump(&mut self) {
        for tab in TABS {
            let changed = self.panes[tab.index()].pump();
            if changed {
                if let Some(info) = &self.panes[tab.index()].exit {
                    self.message = Some(info.message(tab.label()));
                }
                self.dirty = true;
            }
        }
    }

    fn handle_key(&mut self, key: Key) -> Result<bool> {
        let action = route_key(self.mode, key);
        match action {
            Action::Switch(tab) => self.select(tab),
            Action::Cycle => {
                let next = self.active.next();
                self.select(next);
            }
            Action::StartPrefix => {
                self.mode = Mode::Prefix;
                self.message = None;
                self.dirty = true;
            }
            Action::Host(command) => match command {
                HostCommand::Restart => {
                    self.mode = Mode::Normal;
                    self.restart_active();
                }
                HostCommand::Quit => return Ok(true),
                HostCommand::Help => {
                    self.mode = Mode::Help;
                    self.dirty = true;
                }
                HostCommand::Cancel => {
                    self.mode = Mode::Normal;
                    self.dirty = true;
                }
                HostCommand::ForwardPrefix => {
                    self.mode = Mode::Normal;
                    self.dirty = true;
                    self.panes[self.active.index()].send(&encode_key(Key::Ctrl('b')));
                }
                HostCommand::Unknown => {
                    self.mode = Mode::Normal;
                    self.message = Some("unknown host key (r restart, q quit, ? help)".into());
                    self.dirty = true;
                }
            },
            Action::Send(key) => {
                self.panes[self.active.index()].send(&encode_key(key));
                self.message = None;
                self.dirty = true;
            }
        }
        Ok(false)
    }

    fn draw(&mut self) -> Result<()> {
        let footer = match self.mode {
            Mode::Prefix => "Ctrl+b: r restart | q quit | ? help | ESC cancel".to_string(),
            Mode::Help => HELP_TEXT.to_string(),
            Mode::Normal => self.message.clone().unwrap_or_default(),
        };
        let width = usize::from(self.cols.max(1));
        let footer_lines = if footer.is_empty() {
            0
        } else {
            footer.chars().count().div_ceil(width).min(3) as u16
        };
        let Self {
            terminal,
            panes,
            active,
            ..
        } = self;
        terminal
            .draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Min(1),
                        Constraint::Length(footer_lines),
                    ])
                    .split(frame.area());
                render_tabs(frame, chunks[0], panes, *active);
                let screen = panes[active.index()].parser.screen();
                frame.render_widget(PseudoTerminal::new(screen), chunks[1]);
                if footer_lines > 0 {
                    let line = Line::from(Span::styled(
                        footer.to_string(),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ));
                    frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), chunks[2]);
                }
            })
            .map_err(|e| Error::msg(format!("terminal draw: {e}")))?;
        self.dirty = false;
        Ok(())
    }

    fn event_loop(&mut self, shutdown: &AtomicBool) -> Result<ExitCode> {
        self.draw()?;
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return Ok(ExitCode::SUCCESS);
            }
            self.pump();
            if event::poll(POLL_INTERVAL).map_err(|e| Error::msg(format!("event poll: {e}")))? {
                match event::read().map_err(|e| Error::msg(format!("event read: {e}")))? {
                    Event::Key(key) => {
                        if let Some(key) = key_from_event(key) {
                            if self.handle_key(key)? {
                                return Ok(ExitCode::SUCCESS);
                            }
                        }
                    }
                    Event::Resize(cols, rows) => self.resize(cols, rows),
                    Event::Mouse(mouse) if self.active_pane().mouse_enabled => {
                        let bytes = encode_mouse(mouse);
                        self.panes[self.active.index()].send(&bytes);
                    }
                    _ => {}
                }
            }
            self.refresh_mouse_capture()?;
            if self.dirty {
                self.draw()?;
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for pane in &mut self.panes {
            pane.stop();
        }
        let _ = self.persist();
        if self.mouse_capture {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
    }
}

fn render_tabs(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    panes: &[Pane],
    active: Tab,
) {
    let mut spans = vec![Span::raw(" ")];
    for (index, tab) in TABS.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" | "));
        }
        let mut label = tab.label().to_string();
        if panes[tab.index()].exit.is_some() {
            label.push('!');
        }
        let style = if *tab == active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
    }
    spans.push(Span::styled(
        "  Ctrl+b ? help",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn command_for(
    tab: Tab,
    loaded: &LoadedConfig,
    cli_override: Option<&str>,
    _saved: &TopState,
) -> Result<Vec<String>> {
    let name = tab.label();
    if let Some(raw) = cli_override {
        return split_command(raw);
    }
    if let Some(argv) = loaded.top.command_for(name) {
        return Ok(argv.to_vec());
    }
    // Saved argv is deliberately ignored, even when it came from a trusted
    // earlier run. Commands come only from the CLI or the trusted config.
    let component = crate::component::resolve_component(name, &loaded.components)?;
    let binary = component
        .binaries
        .first()
        .ok_or_else(|| Error::msg(format!("component '{name}' has no binary")))?;
    let path = crate::install::install_dir(loaded.auto_update.prefix.as_deref()).join(binary);
    let mut argv = vec![path.to_string_lossy().into_owned(), "top".to_string()];
    if let Some(config) = component.config_path() {
        argv.push("--config".to_string());
        argv.push(config.to_string_lossy().into_owned());
    }
    Ok(argv)
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            Show,
            LeaveAlternateScreen
        );
        let _ = ratatui::try_restore();
    }
}

#[cfg(unix)]
fn register_signals() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        let _ = signal_hook::flag::register(signal, Arc::clone(&flag));
    }
    flag
}

#[cfg(not(unix))]
fn register_signals() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

pub fn run(
    loaded: &LoadedConfig,
    reiki_cmd: Option<&str>,
    lu_cmd: Option<&str>,
) -> Result<ExitCode> {
    let shutdown = register_signals();
    let state_path = state::top_state_path_for_config(
        loaded.auto_update.prefix.as_deref(),
        Some(&loaded.config_path),
    );
    let saved = load_state(&state_path);
    let active = saved.active_tab();
    let terminal = ratatui::try_init()
        .map_err(|e| Error::msg(format!("cannot initialize top terminal: {e}")))?;
    let _guard = TerminalGuard;
    let mut host = Host::new(
        terminal, loaded, state_path, saved, active, reiki_cmd, lu_cmd,
    )?;
    let code = host.event_loop(&shutdown)?;
    drop(host);
    drop(_guard);
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_keys_route_to_child_except_host_bindings() {
        assert_eq!(
            route_key(Mode::Normal, Key::Char('+')),
            Action::Send(Key::Char('+'))
        );
        assert_eq!(
            route_key(Mode::Normal, Key::Ctrl('a')),
            Action::Send(Key::Ctrl('a'))
        );
        assert_eq!(
            route_key(Mode::Normal, Key::Alt('1')),
            Action::Switch(Tab::Reiki)
        );
        assert_eq!(
            route_key(Mode::Normal, Key::Alt('2')),
            Action::Switch(Tab::Lu)
        );
        assert_eq!(
            route_key(Mode::Normal, Key::F(1)),
            Action::Switch(Tab::Reiki)
        );
        assert_eq!(route_key(Mode::Normal, Key::F(2)), Action::Switch(Tab::Lu));
        assert_eq!(route_key(Mode::Normal, Key::CtrlTab), Action::Cycle);
        assert_eq!(route_key(Mode::Normal, Key::Ctrl('b')), Action::StartPrefix);
    }

    #[test]
    fn prefix_keys_route_to_host_commands() {
        assert_eq!(
            route_key(Mode::Prefix, Key::Char('r')),
            Action::Host(HostCommand::Restart)
        );
        assert_eq!(
            route_key(Mode::Prefix, Key::Char('q')),
            Action::Host(HostCommand::Quit)
        );
        assert_eq!(
            route_key(Mode::Prefix, Key::Char('?')),
            Action::Host(HostCommand::Help)
        );
        assert_eq!(
            route_key(Mode::Prefix, Key::Esc),
            Action::Host(HostCommand::Cancel)
        );
        assert_eq!(
            route_key(Mode::Prefix, Key::Ctrl('b')),
            Action::Host(HostCommand::ForwardPrefix)
        );
        assert_eq!(
            route_key(Mode::Prefix, Key::Char('x')),
            Action::Host(HostCommand::Unknown)
        );
        assert_eq!(
            route_key(Mode::Help, Key::Char('q')),
            Action::Host(HostCommand::Cancel)
        );
    }

    #[test]
    fn tab_state_machine_cycles_and_restores() {
        assert_eq!(Tab::Reiki.next(), Tab::Lu);
        assert_eq!(Tab::Lu.next(), Tab::Reiki);
        assert_eq!(Tab::from_name("lu"), Some(Tab::Lu));
        assert_eq!(Tab::from_name("missing"), None);
        let mut saved = TopState {
            active: Some("lu".into()),
            children: BTreeMap::from([(
                "lu".to_string(),
                ChildState {
                    restart_count: 3,
                    last_args: vec!["lu".into(), "top".into()],
                },
            )]),
        };
        assert_eq!(saved.active_tab(), Tab::Lu);
        saved.active = Some("bogus".into());
        assert_eq!(saved.active_tab(), Tab::Reiki);
        assert_eq!(saved.children["lu"].restart_count, 3);
    }

    #[test]
    fn command_override_is_split_without_a_shell() {
        let argv = split_command("reiki top --config '/etc/my reiki.toml' --url=http://x").unwrap();
        assert_eq!(
            argv,
            [
                "reiki",
                "top",
                "--config",
                "/etc/my reiki.toml",
                "--url=http://x"
            ]
        );
        assert!(split_command("\"unterminated").is_err());
        assert!(split_command("   ").is_err());
    }

    #[test]
    fn saved_argv_is_never_used_as_a_child_command() {
        let td = tempfile::tempdir().unwrap();
        let config = td.path().join("tuishou.toml");
        std::fs::write(&config, "").unwrap();
        let loaded = crate::config::load_config(Some(&config)).unwrap();
        let saved = TopState {
            active: None,
            children: BTreeMap::from([(
                "lu".to_string(),
                ChildState {
                    restart_count: 0,
                    last_args: vec!["/no/such/payload".to_string()],
                },
            )]),
        };
        let argv = command_for(Tab::Lu, &loaded, None, &saved).unwrap();
        assert_eq!(argv[1], "top");
        assert!(!argv[0].contains("payload"), "{argv:?}");
    }

    #[test]
    fn exit_hint_cannot_carry_terminal_controls() {
        let hint = exit_hint(b"done \x1b]52;c;x\x07 \x9b31mtext", 0);
        assert!(!hint.chars().any(char::is_control), "{hint}");
        assert!(!hint.contains('\u{1b}'), "{hint}");
    }

    #[test]
    fn mouse_mode_follows_last_request() {
        let mut mode = false;
        mode = update_mouse_mode(mode, b"normal \x1b[?1000h\x1b[?1002h text");
        assert!(mode);
        mode = update_mouse_mode(mode, b"\x1b[?1000l");
        assert!(!mode);
        mode = update_mouse_mode(mode, b"\x1b[?1006h\x1b[?1006l");
        assert!(!mode);
    }

    #[test]
    fn encoded_input_keeps_modifiers() {
        assert_eq!(encode_key(Key::Ctrl('b')), vec![0x02]);
        assert_eq!(encode_key(Key::Alt('1')), vec![0x1b, b'1']);
        assert_eq!(encode_key(Key::Enter), vec![b'\r']);
        assert_eq!(encode_key(Key::Up), b"\x1b[A");
    }

    #[test]
    fn exit_hints_name_the_tui_feature_and_install_path() {
        let hint = exit_hint(b"error: the `tui` feature is required for `top`", 2);
        assert!(hint.contains("tui"));
        assert!(hint.contains("tuishou install"));
        let hint = exit_hint(b"", 127);
        assert!(hint.contains("missing"));
    }
}
