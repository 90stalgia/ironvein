//! menu.rs — the front-end shell: main menu, settings, in-game pause overlay,
//! and the immediate-mode buttons/sliders they're drawn from. Presentation
//! only; it never touches the sim or the network.

use macroquad::prelude::*;

const ACCENT: Color = Color::new(0.86, 0.43, 0.22, 1.0);
const CREAM: Color = Color::new(0.85, 0.79, 0.62, 1.0);
const PANEL: Color = Color::new(0.08, 0.08, 0.10, 0.96);

// ---------------------------------------------------------------------------
// Persisted settings (localStorage on wasm, saves/settings.cfg on native)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
extern "C" {
    fn ivn_pref_save(kp: *const u8, kl: usize, vp: *const u8, vl: usize);
    fn ivn_pref_load(kp: *const u8, kl: usize, out: *mut u8, cap: usize) -> i32;
}

fn pref_save(key: &str, val: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        ivn_pref_save(key.as_ptr(), key.len(), val.as_ptr(), val.len());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = std::fs::create_dir_all("saves");
        let _ = std::fs::write(format!("saves/{key}.cfg"), val);
    }
}

fn pref_load(key: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut buf = [0u8; 512];
        let n = unsafe { ivn_pref_load(key.as_ptr(), key.len(), buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            return None;
        }
        return core::str::from_utf8(&buf[..n as usize]).ok().map(|s| s.to_string());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::read_to_string(format!("saves/{key}.cfg")).ok()
    }
}

#[derive(Clone)]
pub struct Settings {
    pub master: f32,
    pub music: f32,
    pub sfx: f32,
    pub edge_scroll: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { master: 0.9, music: 0.7, sfx: 0.9, edge_scroll: true }
    }
}

impl Settings {
    pub fn load() -> Settings {
        let mut s = Settings::default();
        if let Some(text) = pref_load("settings") {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim();
                    match k.trim() {
                        "master" => s.master = v.parse().unwrap_or(s.master),
                        "music" => s.music = v.parse().unwrap_or(s.music),
                        "sfx" => s.sfx = v.parse().unwrap_or(s.sfx),
                        "edge_scroll" => s.edge_scroll = v == "1",
                        _ => {}
                    }
                }
            }
        }
        s
    }

    pub fn save(&self) {
        let text = format!(
            "master={}\nmusic={}\nsfx={}\nedge_scroll={}\n",
            self.master,
            self.music,
            self.sfx,
            if self.edge_scroll { 1 } else { 0 }
        );
        pref_save("settings", &text);
    }

    pub fn apply_audio(&self) {
        crate::audio::set_volumes(self.master, self.music, self.sfx);
    }
}

// ---------------------------------------------------------------------------
// Immediate-mode widgets
// ---------------------------------------------------------------------------

fn centered(text: &str, cx: f32, y: f32, size: f32, color: Color) {
    let d = measure_text(text, None, size as u16, 1.0);
    draw_text(text, cx - d.width * 0.5, y, size, color);
}

/// A themed button. Draws hover/press states and plays a click blip; returns
/// true on the frame it's clicked.
pub fn button(r: Rect, label: &str, m: Vec2, click: bool) -> bool {
    let hot = r.contains(m);
    let down = hot && is_mouse_button_down(MouseButton::Left);
    let bg = if down {
        Color::new(0.22, 0.15, 0.10, 0.97)
    } else if hot {
        Color::new(0.17, 0.14, 0.13, 0.95)
    } else {
        Color::new(0.11, 0.11, 0.13, 0.90)
    };
    draw_rectangle(r.x, r.y, r.w, r.h, bg);
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, if hot { ACCENT } else { Color::new(0.38, 0.30, 0.25, 0.9) });
    if hot {
        draw_rectangle(r.x, r.y, 4.0, r.h, ACCENT); // accent rail
    }
    let fs = 26.0;
    let d = measure_text(label, None, fs as u16, 1.0);
    let tc = if hot { Color::new(1.0, 0.92, 0.78, 1.0) } else { CREAM };
    draw_text(label, r.x + (r.w - d.width) * 0.5, r.y + (r.h + d.height) * 0.5, fs, tc);
    let clicked = hot && click;
    if clicked {
        crate::audio::sfx("radar", 0.5);
    }
    clicked
}

/// A horizontal 0..1 slider. Drags while the mouse is held over its row;
/// returns the (possibly updated) value.
pub fn slider(r: Rect, label: &str, value: f32, m: Vec2, down: bool) -> f32 {
    draw_text(label, r.x, r.y - 6.0, 20.0, CREAM);
    let ty = r.y + r.h * 0.5;
    draw_line(r.x, ty, r.x + r.w, ty, 4.0, Color::new(0.30, 0.30, 0.34, 1.0));
    let fill = r.w * value.clamp(0.0, 1.0);
    draw_line(r.x, ty, r.x + fill, ty, 4.0, ACCENT);
    let in_row = m.x >= r.x - 16.0 && m.x <= r.x + r.w + 16.0 && m.y >= r.y - 10.0 && m.y <= r.y + r.h + 10.0;
    draw_circle(r.x + fill, ty, if in_row { 9.0 } else { 7.0 }, CREAM);
    draw_text(&format!("{}%", (value * 100.0).round() as i32), r.x + r.w + 14.0, ty + 6.0, 18.0, CREAM);
    if down && in_row {
        ((m.x - r.x) / r.w).clamp(0.0, 1.0)
    } else {
        value
    }
}

/// A dark gradient backdrop with drifting embers, for the menu screens.
pub fn backdrop() {
    draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.05, 0.06, 0.08, 1.0));
    // a faint vignette band toward the bottom
    draw_rectangle(0.0, screen_height() * 0.6, screen_width(), screen_height() * 0.4, Color::new(0.0, 0.0, 0.0, 0.25));
    let t = get_time() as f32;
    let h = screen_height();
    for i in 0..48 {
        let fi = i as f32;
        let x = (fi * 101.3 + t * (6.0 + (fi * 0.37).sin().abs() * 10.0)) % screen_width();
        let y = h - ((t * (10.0 + (fi * 0.21).cos().abs() * 14.0) + fi * 73.0) % (h + 40.0));
        let a = 0.06 + 0.10 * ((t * 1.4 + fi).sin() * 0.5 + 0.5);
        draw_circle(x, y, 1.4, Color::new(0.92, 0.52, 0.26, a));
    }
}

fn title(cx: f32, y: f32) {
    let fs = 86.0;
    let d = measure_text("IRONVEIN", None, fs as u16, 1.0);
    draw_text("IRONVEIN", cx - d.width * 0.5 + 3.0, y + 3.0, fs, Color::new(0.0, 0.0, 0.0, 0.6));
    draw_text("IRONVEIN", cx - d.width * 0.5, y, fs, ACCENT);
    centered("a peer-to-peer persistent-world RTS", cx, y + 32.0, 20.0, CREAM);
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

/// The settings panel (used by both the main menu and the in-game pause).
/// Applies volume changes live; returns true on the frame "BACK" is clicked.
pub fn settings_panel(s: &mut Settings, m: Vec2, click: bool, down: bool) -> bool {
    let (pw, ph) = (480.0, 320.0);
    let (px, py) = ((screen_width() - pw) * 0.5, (screen_height() - ph) * 0.5);
    draw_rectangle(px, py, pw, ph, PANEL);
    draw_rectangle_lines(px, py, pw, ph, 2.0, ACCENT);
    centered("SETTINGS", px + pw * 0.5, py + 44.0, 30.0, CREAM);

    let (sx, sw) = (px + 44.0, pw - 150.0);
    let mut sy = py + 94.0;
    let before = (s.master, s.music, s.sfx);
    s.master = slider(Rect::new(sx, sy, sw, 18.0), "Master", s.master, m, down);
    sy += 58.0;
    s.music = slider(Rect::new(sx, sy, sw, 18.0), "Music", s.music, m, down);
    sy += 58.0;
    s.sfx = slider(Rect::new(sx, sy, sw, 18.0), "Effects", s.sfx, m, down);
    if (s.master, s.music, s.sfx) != before {
        s.apply_audio();
    }

    let (bw, bh) = (160.0, 42.0);
    button(Rect::new(px + (pw - bw) * 0.5, py + ph - 58.0, bw, bh), "BACK", m, click)
}

#[allow(dead_code)] // Quit is native-only (no window to close in the browser)
pub enum StartChoice {
    /// Begin a game. `survival` picks the mode, `difficulty` 0/1/2, and
    /// `continue_run` loads the existing save for that mode instead of a new one.
    Start { survival: bool, difficulty: u8, continue_run: bool },
    /// Load a manual save slot (1..=SAVE_SLOTS).
    LoadSlot(usize),
    Quit,
}

enum Page {
    Main,
    /// difficulty / continue picker for the chosen mode
    Mode { survival: bool },
    /// the manual load-slot picker
    Load,
}

/// How many manual save slots the save/load screen offers.
pub const SAVE_SLOTS: usize = 6;

pub fn slot_iv_path(n: usize) -> String {
    format!("saves/save{n}.iv")
}
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn slot_meta_path(n: usize) -> String {
    format!("saves/save{n}.meta")
}

/// A one-line description of what's in slot `n`, or None if the slot is empty.
pub fn slot_label(n: usize) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = n;
        None // the browser has no disk — slots are native-only
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !std::path::Path::new(&slot_iv_path(n)).exists() {
            return None;
        }
        Some(std::fs::read_to_string(slot_meta_path(n)).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "saved game".into()))
    }
}

/// Write a slot's human label (called by the client right after it writes the .iv).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn write_slot_label(n: usize, label: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = std::fs::create_dir_all("saves");
        let _ = std::fs::write(slot_meta_path(n), label);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (n, label);
    }
}

/// What a save/load slot screen returned this frame.
pub enum SlotPick {
    Pick(usize),
    Back,
    None,
}

/// A grid of save slots: each shows its contents (or "empty"). In `save` mode
/// every slot is clickable (to write/overwrite); in load mode only filled ones.
/// Returns the picked slot, Back, or None. Caller draws the backdrop.
pub fn slot_screen(title: &str, save: bool, m: Vec2, click: bool) -> SlotPick {
    let cx = screen_width() * 0.5;
    centered(title, cx, screen_height() * 0.18, 34.0, ACCENT);
    let (bw, bh) = (440.0, 46.0);
    let bx = cx - bw * 0.5;
    let mut by = screen_height() * 0.26;
    let mut picked = SlotPick::None;
    for n in 1..=SAVE_SLOTS {
        let r = Rect::new(bx, by, bw, bh);
        let label = slot_label(n);
        let filled = label.is_some();
        let clickable = save || filled;
        let hot = clickable && r.contains(m);
        let bg = if hot {
            Color::new(0.18, 0.15, 0.13, 0.96)
        } else {
            Color::new(0.10, 0.10, 0.12, 0.92)
        };
        draw_rectangle(r.x, r.y, r.w, r.h, bg);
        draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, if hot { ACCENT } else { Color::new(0.34, 0.30, 0.26, 0.9) });
        draw_text(&format!("Slot {n}"), r.x + 14.0, r.y + 29.0, 22.0, CREAM);
        let info = label.unwrap_or_else(|| "— empty —".into());
        let ic = if filled { Color::new(0.7, 0.88, 0.7, 1.0) } else { Color::new(0.5, 0.5, 0.55, 1.0) };
        let iw = measure_text(&info, None, 18, 1.0).width;
        draw_text(&info, r.x + r.w - iw - 14.0, r.y + 28.0, 18.0, ic);
        if hot && click {
            crate::audio::sfx("radar", 0.5);
            picked = SlotPick::Pick(n);
        }
        by += bh + 10.0;
    }
    by += 6.0;
    if button(Rect::new(cx - 110.0, by, 220.0, 44.0), "BACK", m, click) {
        return SlotPick::Back;
    }
    if save {
        centered("click a slot to save (overwrites it)", cx, by + 62.0, 16.0, Color::new(0.6, 0.58, 0.5, 1.0));
    }
    picked
}

/// The main menu. Blocks (its own frame loop) until the player picks a mode +
/// difficulty (or Continue) — fine because there's no live session yet.
/// `surv_save`/`skir_save` say whether a resumable save exists for each mode.
pub async fn start_screen(settings: &mut Settings, region: &str, surv_save: bool, skir_save: bool) -> StartChoice {
    let mut in_settings = false;
    let mut page = Page::Main;
    loop {
        let (mx, my) = mouse_position();
        let m = vec2(mx, my);
        let click = is_mouse_button_pressed(MouseButton::Left);
        let down = is_mouse_button_down(MouseButton::Left);

        clear_background(BLACK);
        backdrop();
        let cx = screen_width() * 0.5;
        // the slot list owns the whole screen; everything else gets the logo
        if !matches!(page, Page::Load) {
            title(cx, screen_height() * 0.24);
        }

        if in_settings {
            if settings_panel(settings, m, click, down) {
                in_settings = false;
                settings.save();
            }
            next_frame().await;
            continue;
        }

        let (bw, bh) = (300.0, 50.0);
        let bx = cx - bw * 0.5;
        match page {
            Page::Main => {
                let mut by = screen_height() * 0.42;
                if button(Rect::new(bx, by, bw, bh), "SURVIVAL", m, click) {
                    page = Page::Mode { survival: true };
                }
                centered("hold out, alone, against the night", cx, by + bh + 14.0, 16.0, Color::new(0.62, 0.6, 0.52, 1.0));
                by += bh + 32.0;
                if button(Rect::new(bx, by, bw, bh), "SKIRMISH", m, click) {
                    page = Page::Mode { survival: false };
                }
                centered("versus a rival colony", cx, by + bh + 14.0, 16.0, Color::new(0.62, 0.6, 0.52, 1.0));
                by += bh + 32.0;
                // LOAD GAME — only when at least one slot holds a save
                let any_slot = (1..=SAVE_SLOTS).any(|n| slot_label(n).is_some());
                if any_slot && button(Rect::new(bx, by, bw, bh), "LOAD GAME", m, click) {
                    page = Page::Load;
                }
                if any_slot {
                    by += bh + 18.0;
                }
                let sw = (bw - 14.0) * 0.5;
                if button(Rect::new(bx, by, sw, bh), "SETTINGS", m, click) {
                    in_settings = true;
                }
                #[cfg(not(target_arch = "wasm32"))]
                if button(Rect::new(bx + sw + 14.0, by, sw, bh), "QUIT", m, click) {
                    return StartChoice::Quit;
                }
                #[cfg(target_arch = "wasm32")]
                {
                    centered(&format!("Sector {region}"), bx + sw + 14.0 + sw * 0.5, by + bh * 0.5 + 6.0, 18.0, CREAM);
                }
                let _ = region;
            }
            Page::Mode { survival } => {
                let has_save = if survival { surv_save } else { skir_save };
                centered(if survival { "SURVIVAL" } else { "SKIRMISH" }, cx, screen_height() * 0.36, 30.0, CREAM);
                let mut by = screen_height() * 0.42;
                if has_save && button(Rect::new(bx, by, bw, bh), "CONTINUE", m, click) {
                    return StartChoice::Start { survival, difficulty: 1, continue_run: true };
                }
                if has_save {
                    centered("resume your saved run", cx, by + bh + 13.0, 15.0, Color::new(0.6, 0.58, 0.5, 1.0));
                    by += bh + 30.0;
                }
                for (i, (label, pitch)) in [
                    ("EASY", "fewer, slower monsters"),
                    ("NORMAL", "the standard threat"),
                    ("HARD", "a relentless, swarming night"),
                ]
                .iter()
                .enumerate()
                {
                    if button(Rect::new(bx, by, bw, bh), label, m, click) {
                        return StartChoice::Start { survival, difficulty: i as u8, continue_run: false };
                    }
                    centered(pitch, cx, by + bh + 13.0, 15.0, Color::new(0.55, 0.53, 0.46, 1.0));
                    by += bh + 28.0;
                }
                if button(Rect::new(bx, by, bw, bh), "BACK", m, click) {
                    page = Page::Main;
                }
            }
            Page::Load => match slot_screen("LOAD GAME", false, m, click) {
                SlotPick::Pick(n) => return StartChoice::LoadSlot(n),
                SlotPick::Back => page = Page::Main,
                SlotPick::None => {}
            },
        }
        centered("F1 in-game for controls  ·  M for the world map", cx, screen_height() - 26.0, 18.0, Color::new(0.6, 0.58, 0.5, 1.0));
        next_frame().await;
    }
}

// ---------------------------------------------------------------------------
// In-game pause overlay (drawn on top of the live game; never stops the sim)
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // SaveSlots is native-only (no disk in browser)
pub enum Overlay {
    None,
    Pause,
    Settings,
    SaveSlots,
}

#[allow(dead_code)] // Quit/SaveSlot are native-only
pub enum OverlayResult {
    None,
    Quit,
    SaveSlot(usize),
}

/// Draw + handle the pause overlay for one frame. The caller keeps pumping the
/// session/network underneath, so this is safe in multiplayer (it's a local
/// view, not a real pause). `esc` should already exclude the frame it opened.
pub fn overlay_frame(overlay: &mut Overlay, settings: &mut Settings, m: Vec2, click: bool, down: bool, esc: bool) -> OverlayResult {
    draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, 0.55));
    let cx = screen_width() * 0.5;
    match *overlay {
        Overlay::Settings => {
            if settings_panel(settings, m, click, down) || esc {
                *overlay = Overlay::Pause;
                settings.save();
            }
            OverlayResult::None
        }
        Overlay::SaveSlots => {
            match slot_screen("SAVE GAME", true, m, click) {
                SlotPick::Pick(n) => {
                    *overlay = Overlay::Pause;
                    return OverlayResult::SaveSlot(n);
                }
                SlotPick::Back => *overlay = Overlay::Pause,
                SlotPick::None => {}
            }
            if esc {
                *overlay = Overlay::Pause;
            }
            OverlayResult::None
        }
        Overlay::Pause => {
            centered("PAUSED", cx, screen_height() * 0.30, 40.0, CREAM);
            let (bw, bh) = (240.0, 48.0);
            let bx = cx - bw * 0.5;
            let mut by = screen_height() * 0.40;
            #[allow(unused_mut)] // QUIT GAME (which mutates this) is native-only
            let mut res = OverlayResult::None;
            if button(Rect::new(bx, by, bw, bh), "RESUME", m, click) || esc {
                *overlay = Overlay::None;
            }
            by += bh + 12.0;
            #[cfg(not(target_arch = "wasm32"))]
            if button(Rect::new(bx, by, bw, bh), "SAVE GAME", m, click) {
                *overlay = Overlay::SaveSlots;
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                by += bh + 12.0;
            }
            if button(Rect::new(bx, by, bw, bh), "SETTINGS", m, click) {
                *overlay = Overlay::Settings;
            }
            by += bh + 12.0;
            #[cfg(not(target_arch = "wasm32"))]
            if button(Rect::new(bx, by, bw, bh), "QUIT GAME", m, click) {
                res = OverlayResult::Quit;
            }
            #[cfg(target_arch = "wasm32")]
            let _ = by;
            res
        }
        Overlay::None => OverlayResult::None,
    }
}
