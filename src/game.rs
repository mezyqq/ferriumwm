//! Game mode (Ctrl+Shift+F): the focused window goes fullscreen instantly, FeWM
//! stops animating and drawing effects, and only the game is composited — so its
//! buffer can go straight to the display. Commands from the config hide bars,
//! enable DND and switch the power profile; everything is restored on exit.

use tracing::info;

use crate::{layout::WinId, state::Fe};

pub struct GameMode {
    pub win: WinId,
    anims_were: bool,
}

impl Fe {
    pub fn game_window(&self) -> Option<WinId> {
        self.game.as_ref().map(|g| g.win)
    }

    pub fn toggle_game_mode(&mut self) {
        if self.game.is_some() {
            self.exit_game_mode();
            return;
        }
        let Some(id) = self.focus.filter(|f| self.windows.contains_key(f)) else { return };
        let anims_were = self.cfg.animations.enabled;
        self.cfg.animations.enabled = false;
        self.game = Some(GameMode { win: id, anims_were });
        if let Some(w) = self.windows.get_mut(&id) {
            w.open = None;
        }
        self.closing.clear();
        for o in &mut self.outputs {
            o.switch = None;
        }
        if !self.windows[&id].fullscreen {
            self.set_fullscreen(id, true);
        }
        let cmd = self.cfg.misc.game_mode_on.clone();
        self.spawn(&cmd);
        info!("игровой режим: включён ({})", self.windows[&id].app_id);
        crate::ipc::event(self, "game-mode");
        self.relayout_all();
        self.queue_redraw();
    }

    pub fn exit_game_mode(&mut self) {
        let Some(g) = self.game.take() else { return };
        self.cfg.animations.enabled = g.anims_were;
        if self.windows.get(&g.win).is_some_and(|w| w.fullscreen) {
            self.set_fullscreen(g.win, false);
        }
        let cmd = self.cfg.misc.game_mode_off.clone();
        self.spawn(&cmd);
        info!("игровой режим: выключен");
        crate::ipc::event(self, "game-mode");
        self.relayout_all();
        self.queue_redraw();
    }

    /// Leave game mode when its window closed or left fullscreen.
    pub fn check_game_mode(&mut self) {
        if let Some(id) = self.game_window() {
            if !self.windows.get(&id).is_some_and(|w| w.fullscreen) {
                self.exit_game_mode();
            }
        }
    }

    /// After a config reload: keep animations off while the game runs.
    pub fn reapply_game_mode(&mut self) {
        if let Some(g) = &mut self.game {
            g.anims_were = self.cfg.animations.enabled;
            self.cfg.animations.enabled = false;
        }
    }
}
